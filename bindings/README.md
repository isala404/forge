# Forge language bindings

This directory contains first-party package scaffolds for Forge across package
registries. Rust, Node.js, and Python are the active bindings today. The other
packages are minimal metadata releases that reserve the official first-party
package coordinates while their full bindings are prepared.

Package names:

- Rust: `forgelib` on crates.io
- Node.js: `forgelib` on npm
- Python: `forgelib` on PyPI
- Dart: `forgelib` on pub.dev
- Ruby: `forgelib` on RubyGems
- .NET: `ForgeLib` on NuGet
- Java/Kotlin: `io.github.isala404:forgelib` on Maven Central
- PHP: `isala404/forgelib` on Packagist
- Swift: `ForgeLib` via Swift Package Manager and CocoaPods
- Elixir: `forgelib` on Hex
- Haskell: `forgelib` on Hackage
- JavaScript runtime registry: `@isala404/forgelib` on JSR
- R: `forgelib` on CRAN
- Go: `github.com/isala404/forge/bindings/go`
- Container image: `ghcr.io/isala404/forgelib`

Reserved 0.0.1 releases:

- crates.io: `forgelib` `0.0.1`
- npm: `forgelib` `0.0.1`
- pub.dev: `forgelib` `0.0.1`
- JSR: `@isala404/forgelib` `0.0.1`
- Hex: `forgelib` `0.0.1`
- GHCR: `ghcr.io/isala404/forgelib:0.0.1`

Ready for first publish once registry credentials or registry approval are completed:

- PyPI, RubyGems, NuGet, Maven Central, Packagist, CocoaPods, and Hackage.
- CRAN, Go, and SwiftPM do not have token-based publish steps in this
  placeholder workflow; their package scaffolds are checked by CI.
