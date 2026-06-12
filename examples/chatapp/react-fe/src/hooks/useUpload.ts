import { useCallback, useState } from 'react'
import { useMutation } from 'urql'
import { RequestUploadMutation } from '../graphql/operations'
import { putToUrl, UploadError } from '../lib/urql'
import { errorMessage } from '../lib/errors'

export type UploadResult = { key: string } | { error: string }

// Presign a PUT, push the bytes straight to blob storage, and hand back the
// media key for sendMessage. The GraphQL API never sees the file body.
export function useUpload(chatId: string) {
  const [, requestUpload] = useMutation(RequestUploadMutation)
  const [uploading, setUploading] = useState(false)

  // One full attempt: mint a fresh presigned ticket and push the bytes. The
  // ticket is a short-lived single-use URL, so a retry must re-presign rather
  // than reuse it. The size guard runs against each ticket's maxBytes.
  const attempt = useCallback(
    async (file: File): Promise<UploadResult> => {
      const ticket = await requestUpload({ chatId })
      if (ticket.error || !ticket.data) {
        return { error: errorMessage(ticket.error) }
      }
      const { uploadUrl, key, maxBytes } = ticket.data.requestUpload
      if (file.size > maxBytes) {
        return {
          error: `File is too large. Max ${(maxBytes / 1024 / 1024).toFixed(0)} MiB.`,
        }
      }
      await putToUrl(uploadUrl, file)
      return { key }
    },
    [chatId, requestUpload],
  )

  const upload = useCallback(
    async (file: File): Promise<UploadResult> => {
      setUploading(true)
      try {
        try {
          return await attempt(file)
        } catch (e) {
          // A 403 means the signature expired or was rejected; re-presign and
          // retry once. The client-side size guard keeps this from looping on a
          // genuinely too-large file. Any other failure is surfaced as-is.
          if (e instanceof UploadError && e.status === 403) {
            return await attempt(file)
          }
          throw e
        }
      } catch (e) {
        return { error: e instanceof Error ? e.message : 'Upload failed.' }
      } finally {
        setUploading(false)
      }
    },
    [attempt],
  )

  return { upload, uploading }
}
