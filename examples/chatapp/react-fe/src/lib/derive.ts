import { readFragment, type FragmentType } from './fragments'
import {
  ChatSummary,
  MediaFields,
  MessageFields,
  ReceiptFields,
  UserFields,
} from '../graphql/operations'
import type {
  ChatSummaryFragment,
  MediaFieldsFragment,
  MessageFieldsFragment,
  ReceiptFieldsFragment,
  UserFieldsFragment,
} from '../gql/graphql'
import type { DeliveryState } from '../components/ui/StatusTicks'

export type Chat = ChatSummaryFragment
export type Message = MessageFieldsFragment
export type Receipt = ReceiptFieldsFragment
export type User = UserFieldsFragment
export type Media = MediaFieldsFragment

export function unmaskUser(u: FragmentType<typeof UserFields>): User {
  return readFragment(UserFields, u)
}

export function unmaskMessage(m: FragmentType<typeof MessageFields>): Message {
  return readFragment(MessageFields, m)
}

export function unmaskReceipt(r: FragmentType<typeof ReceiptFields>): Receipt {
  return readFragment(ReceiptFields, r)
}

export function unmaskChat(c: FragmentType<typeof ChatSummary>): Chat {
  return readFragment(ChatSummary, c)
}

export function unmaskMedia(m: FragmentType<typeof MediaFields>): Media {
  return readFragment(MediaFields, m)
}

export function chatMembers(chat: Chat): User[] {
  return chat.members.map(unmaskUser)
}

// Direct chats have no title; the UI shows the other member. Falls back to "you"
// when a direct chat somehow resolves to a single (self) member.
export function chatDisplayName(chat: Chat, selfId: string): string {
  if (chat.title) return chat.title
  const others = chatMembers(chat).filter((m) => m.id !== selfId)
  if (others.length === 0) return 'You'
  return others.map((m) => m.displayName).join(', ')
}

export function chatPeers(chat: Chat, selfId: string): User[] {
  return chatMembers(chat).filter((m) => m.id !== selfId)
}

export function isGroup(chat: Chat): boolean {
  return chat.kind === 'GROUP'
}

// A direct chat is "online" when the single peer is online.
export function directPeerOnline(chat: Chat, selfId: string): boolean | null {
  if (isGroup(chat)) return null
  const peer = chatPeers(chat, selfId)[0]
  return peer?.online ?? null
}

export function lastMessage(chat: Chat): Message | null {
  return chat.lastMessage ? unmaskMessage(chat.lastMessage) : null
}

export function previewText(msg: Message | null): string {
  if (!msg) return 'No messages yet'
  if (msg.body) return msg.body
  if (msg.media) return 'Attachment'
  return ''
}

// Delivery state for the sender's own message, derived from the receipts of the
// other members: read once all have readAt, delivered once all have deliveredAt,
// otherwise sent.
export function deliveryState(receipts: Receipt[], selfId: string): DeliveryState {
  const others = receipts.filter((r) => unmaskUser(r.user).id !== selfId)
  if (others.length === 0) return 'sent'
  if (others.every((r) => r.readAt)) return 'read'
  if (others.every((r) => r.deliveredAt)) return 'delivered'
  return 'sent'
}
