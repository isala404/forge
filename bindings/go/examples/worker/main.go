package main

import (
	"context"
	"errors"
	"flag"
	"log"
	"os/signal"
	"syscall"
	"time"

	forge "github.com/isala404/forge/bindings/go"
)

func main() {
	migrateOnly := flag.Bool("migrate", false, "apply Forge migrations and exit")
	flag.Parse()
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	if *migrateOnly {
		reports, err := forge.MigrateFrom(ctx, "forge.toml")
		if err != nil {
			log.Fatal(err)
		}
		for _, report := range reports {
			log.Printf("%s: %s (%s)", report.Target, report.State, report.Message)
			if report.State != "applied" {
				log.Fatal("Forge migration did not complete")
			}
		}
		return
	}

	client, err := forge.InitFrom(ctx, "forge.toml")
	if err != nil {
		log.Fatal(err)
	}
	defer func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if err := client.Close(shutdownCtx); err != nil {
			log.Printf("Forge shutdown failed: %v", err)
		}
	}()

	if _, err := client.KVSet(ctx, "worker:started", []byte(time.Now().UTC().Format(time.RFC3339)), forge.SetOptions{}); err != nil {
		log.Fatal(err)
	}
	if _, err := client.Enqueue(ctx, "emails", []byte("welcome:user-42"), forge.EnqueueOptions{Priority: forge.PriorityHigh, ConcurrencyKey: "provider:mail"}); err != nil {
		log.Fatal(err)
	}
	if err := client.ScheduleCron(ctx, "hourly-digest", "0 * * * *", "emails", []byte("digest:all"), forge.ScheduleOptions{MisfirePolicy: forge.MisfireCatchUp, MaxCatchUp: 3}); err != nil {
		log.Fatal(err)
	}
	go func() {
		ticker := time.NewTicker(30 * time.Second)
		defer ticker.Stop()
		for {
			if _, err := client.RunSchedulerOnce(ctx, 1000); err != nil {
				log.Printf("scheduler tick failed: %v", err)
			} else if diagnostics, err := client.SchedulerDiagnostics(ctx); err == nil && diagnostics.DueCount > 0 {
				log.Printf("scheduler remains behind: due=%d lag_ms=%v", diagnostics.DueCount, diagnostics.LagMs)
			}
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
			}
		}
	}()

	err = client.RunWorker(ctx, "emails", func(ctx context.Context, job forge.Job) error {
		log.Printf("processing job %s: %s", job.ID, job.Payload)
		return nil
	}, forge.WorkerOptions{
		Concurrency:            2,
		Visibility:             30 * time.Second,
		HeartbeatCadence:       10 * time.Second,
		RetryBackoff:           250 * time.Millisecond,
		DrainDeadline:          10 * time.Second,
		Identity:               "email-worker",
		ConcurrencyLimitPerKey: 1,
		OnError: func(err error) {
			var forgeErr *forge.Error
			if errors.As(err, &forgeErr) {
				log.Printf("Forge operation failed: code=%s retryable=%t", forgeErr.Code, forgeErr.Retryable)
				return
			}
			log.Printf("worker failed: %v", err)
		},
	})
	if err != nil && ctx.Err() == nil {
		log.Fatal(err)
	}
}
