const { existsSync, readFileSync } = require('fs')
const { join } = require('path')

const { platform, arch } = process

function isMusl() {
  if (!process.report || typeof process.report.getReport !== 'function') {
    try {
      const lddPath = require('child_process').execSync('which ldd').toString().trim()
      return readFileSync(lddPath, 'utf8').includes('musl')
    } catch {
      return true
    }
  }

  const { glibcVersionRuntime } = process.report.getReport().header
  return !glibcVersionRuntime
}

function bindingFileName() {
  switch (platform) {
    case 'win32':
      if (arch === 'x64') {
        return 'forgelib.win32-x64-msvc.node'
      }
      break
    case 'darwin':
      if (arch === 'arm64') {
        return 'forgelib.darwin-arm64.node'
      }
      break
    case 'linux':
      if (arch === 'x64') {
        return isMusl() ? 'forgelib.linux-x64-musl.node' : 'forgelib.linux-x64-gnu.node'
      }
      if (arch === 'arm64' && !isMusl()) {
        return 'forgelib.linux-arm64-gnu.node'
      }
      break
  }

  throw new Error(`Unsupported OS or architecture: ${platform} ${arch}`)
}

const bindingFile = bindingFileName()
const bindingPath = join(__dirname, bindingFile)

if (!existsSync(bindingPath)) {
  throw new Error(`Missing native binding ${bindingFile} in forgelib package`)
}

const nativeBinding = require(`./${bindingFile}`)
const { ForgeClient, JsSubscription } = nativeBinding

module.exports.ForgeClient = ForgeClient
module.exports.JsSubscription = JsSubscription
