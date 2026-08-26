package forge

import (
	"context"
	"crypto/sha256"
	"fmt"
	"sync"
	"unicode/utf8"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

const maxPubsubPayloadBytes = 7000

type Subscription struct {
	owner   *Forge
	store   *MemoryStore
	topic   string
	id      uint64
	ch      chan []byte
	pgConn  *pgxpool.Conn
	channel string
	once    sync.Once
}

func (f *Forge) PubsubChannel(ctx context.Context, topic string) (string, error) {
	if err := f.ready(ctx, "pubsub.channel_for"); err != nil {
		return "", err
	}
	if topic == "" {
		return "", forgeError(CodeInvalid, "pubsub.channel_for", "topic cannot be empty")
	}
	if f.mode == ModePostgres {
		return postgresChannel(f.namespace, topic), nil
	}
	return f.scoped(topic), nil
}

func (f *Forge) Publish(ctx context.Context, topic string, payload []byte) error {
	if err := f.ready(ctx, "pubsub.publish"); err != nil {
		return err
	}
	if topic == "" {
		return forgeError(CodeInvalid, "pubsub.publish", "topic cannot be empty")
	}
	if len(payload) > maxPubsubPayloadBytes {
		return forgeError(CodeLimit, "pubsub.publish", "payload exceeds the notification limit")
	}
	if !utf8.Valid(payload) {
		return forgeError(CodeInvalid, "pubsub.publish", "payload must be valid UTF-8")
	}
	if f.mode == ModePostgres {
		_, err := f.postgres(PrimitivePubsub).Exec(ctx, "SELECT pg_notify($1, $2)", postgresChannel(f.namespace, topic), string(payload))
		return postgresError("pubsub.publish", err)
	}
	scoped := f.scoped(topic)
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	for _, channel := range f.store.subscriptions[scoped] {
		copy := append([]byte(nil), payload...)
		select {
		case channel <- copy:
		default:
		}
	}
	return nil
}

func (f *Forge) Subscribe(ctx context.Context, topic string) (*Subscription, error) {
	if err := f.ready(ctx, "pubsub.subscribe"); err != nil {
		return nil, err
	}
	if topic == "" {
		return nil, forgeError(CodeInvalid, "pubsub.subscribe", "topic cannot be empty")
	}
	if f.mode == ModePostgres {
		conn, err := f.postgres(PrimitivePubsub).Acquire(ctx)
		if err != nil {
			return nil, postgresError("pubsub.subscribe", err)
		}
		channel := postgresChannel(f.namespace, topic)
		if _, err := conn.Exec(ctx, "LISTEN "+pgx.Identifier{channel}.Sanitize()); err != nil {
			conn.Release()
			return nil, postgresError("pubsub.subscribe", err)
		}
		subscription := &Subscription{owner: f, pgConn: conn, channel: channel}
		f.registerSubscription(subscription)
		return subscription, nil
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(topic)
	f.store.nextSubID++
	id := f.store.nextSubID
	if f.store.subscriptions[scoped] == nil {
		f.store.subscriptions[scoped] = make(map[uint64]chan []byte)
	}
	channel := make(chan []byte, 64)
	f.store.subscriptions[scoped][id] = channel
	subscription := &Subscription{owner: f, store: f.store, topic: scoped, id: id, ch: channel}
	f.registerSubscription(subscription)
	return subscription, nil
}

func (s *Subscription) Next(ctx context.Context) ([]byte, error) {
	if s.pgConn != nil {
		notification, err := s.pgConn.Conn().WaitForNotification(ctx)
		if err != nil {
			return nil, postgresError("pubsub.receive", err)
		}
		return []byte(notification.Payload), nil
	}
	select {
	case <-ctx.Done():
		return nil, errorWithCause(CodeUnavailable, "pubsub.receive", "memory", "subscription receive was cancelled", ctx.Err())
	case payload, ok := <-s.ch:
		if !ok {
			return nil, nil
		}
		return payload, nil
	}
}

func (s *Subscription) Close() {
	s.once.Do(func() {
		defer s.owner.unregisterSubscription(s)
		if s.pgConn != nil {
			_, _ = s.pgConn.Exec(context.Background(), "UNLISTEN "+pgx.Identifier{s.channel}.Sanitize())
			s.pgConn.Release()
			return
		}
		s.store.mu.Lock()
		defer s.store.mu.Unlock()
		subscribers := s.store.subscriptions[s.topic]
		if channel, ok := subscribers[s.id]; ok {
			delete(subscribers, s.id)
			close(channel)
		}
		if len(subscribers) == 0 {
			delete(s.store.subscriptions, s.topic)
		}
	})
}

func (f *Forge) registerSubscription(subscription *Subscription) {
	f.subscriptionMu.Lock()
	f.activeSubscriptions[subscription] = struct{}{}
	f.subscriptionMu.Unlock()
}

func (f *Forge) unregisterSubscription(subscription *Subscription) {
	f.subscriptionMu.Lock()
	delete(f.activeSubscriptions, subscription)
	f.subscriptionMu.Unlock()
}

func postgresChannel(namespace, topic string) string {
	sum := sha256.Sum256([]byte(namespace + "\x00" + topic))
	return fmt.Sprintf("forge_%x", sum[:24])
}
