import { createContext, useContext } from 'react'

export type Tone = 'info' | 'error' | 'success'
export type Toast = { id: number; message: string; tone: Tone }
export type PushToast = (message: string, tone?: Tone) => void

export const ToastContext = createContext<PushToast>(() => {})

export function useToast(): PushToast {
  return useContext(ToastContext)
}
