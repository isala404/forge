'use strict'

const { createHash } = require('node:crypto')
const { writeFile, unlink } = require('node:fs/promises')
const { tmpdir } = require('node:os')
const { join } = require('node:path')
const { ForgeClient } = require('../../../bindings/node')

const required = (name) => {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required`)
  return value
}

const quoted = (value) => JSON.stringify(value)
const header = (headers, name) => Object.entries(headers).find(([key]) => key.toLowerCase() === name)?.[1]

async function main() {
  const namespace = `s3_node_${Date.now()}`
  const config = `
[forge]
mode = "postgres"
environment = "test"
namespace = ${quoted(namespace)}

[postgres]
url = ${quoted(required('TEST_DATABASE_URL'))}
auto_migrate = true

[blob]
backend = "s3"
bucket = ${quoted(required('S3_TEST_BUCKET'))}
region = ${quoted(process.env.S3_TEST_REGION || 'us-east-1')}
endpoint = ${quoted(required('S3_TEST_ENDPOINT'))}
prefix = "binding-smoke"
access_key = ${quoted(required('S3_TEST_ACCESS_KEY'))}
secret_key = ${quoted(required('S3_TEST_SECRET_KEY'))}
path_style = true
signing_secret = "binding-proxy-secret"
`
  const forge = await ForgeClient.initFromString(config)
  const path = join(tmpdir(), `forge-${namespace}.bin`)
  try {
    await writeFile(path, Buffer.alloc(51 * 1024 * 1024, 7))
    await forge.blobPutFile('streamed.bin', path, { contentType: 'application/octet-stream', createOnly: true })
    const info = await forge.blobHead('streamed.bin')
    if (!info || info.size !== 51 * 1024 * 1024) throw new Error('streamed upload size mismatch')

    const checksum = createHash('sha256').update('hello').digest('hex')
    await forge.blobPutObject('source.txt', Buffer.from('hello'), {
      contentType: 'text/plain', metadata: { owner: 'node' }, createOnly: true,
      cacheControl: 'public, max-age=60', contentDisposition: 'attachment; filename=source.txt',
      checksumSha256: checksum,
    })
    const source = await forge.blobHead('source.txt')
    if (!source || source.checksumSha256 !== checksum) throw new Error('checksum metadata mismatch')
    if ((await forge.blobGetIf('source.txt', source.etag)).state !== 'found') throw new Error('conditional GET mismatch')
    if ((await forge.blobGetIf('source.txt', null, source.etag)).state !== 'not_modified') throw new Error('not-modified GET mismatch')
    const copied = await forge.blobCopy('source.txt', 'copy.txt')
    if (copied.cacheControl !== 'public, max-age=60' || !await forge.blobVerifyChecksumSha256('copy.txt', checksum)) {
      throw new Error('copy metadata or checksum mismatch')
    }

    const upload = await forge.blobCreateMultipart('handled.bin', {
      contentType: 'application/octet-stream', createOnly: true, cacheControl: 'private, max-age=0',
    })
    const first = await forge.blobUploadPart(upload, 1, Buffer.alloc(5 * 1024 * 1024, 3))
    const second = await forge.blobUploadPart(upload, 2, Buffer.from('tail'))
    const completed = await forge.blobCompleteMultipart(upload, [first, second])
    if (completed.size !== 5 * 1024 * 1024 + 4) throw new Error('multipart handle size mismatch')
    const abandoned = await forge.blobCreateMultipart('abandoned.bin')
    await forge.blobAbortMultipart(abandoned)
    await forge.blobAbortMultipart(abandoned)

    const encrypted = await forge.blobPresignNativePut('encrypted.txt', 60, { contentType: 'text/plain', sseAlgorithm: 'AES256' })
    if (header(encrypted.requiredHeaders, 'x-amz-server-side-encryption') !== 'AES256') throw new Error('S3 encryption header was not signed')
    const put = await forge.blobPresignNativePut('native.txt', 60, { contentType: 'text/plain' })
    const putResponse = await fetch(put.url, { method: put.method, headers: put.requiredHeaders, body: 'node-native' })
    if (!putResponse.ok) throw new Error(`native PUT failed with ${putResponse.status}`)
    const get = await forge.blobPresignNativeGet('native.txt', 60)
    const getResponse = await fetch(get.url, { method: get.method, headers: get.requiredHeaders })
    if (!getResponse.ok || await getResponse.text() !== 'node-native') throw new Error('native GET mismatch')

    const proxy = await forge.blobPresignUpload('proxy.txt', 60, 1024)
    if (!await forge.blobVerifyPresign(proxy.method, proxy.key, proxy.expiresEpoch, proxy.maxBytes, proxy.signature)) {
      throw new Error('proxy ticket did not verify')
    }
    await forge.blobDelete('native.txt')
  } finally {
    await forge.close()
    await unlink(path).catch(() => {})
  }
  console.log('PASS  s3/node_package_object_ergonomics')
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : 'Node S3 smoke failed')
  process.exitCode = 1
})
