# Forge language bindings

This directory contains first-party package scaffolds for Forge across package
registries. Rust, Node.js, and Python are the active `1.0.0` bindings. The other
packages are minimal `0.0.1` metadata releases that reserve official first-party
coordinates while their full bindings are prepared. Metadata-only packages do not
share the active bindings' version and must not be published as `1.x`.

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

The coordinated `1.0.0` release targets are:

- crates.io: `forgelib`
- npm: `forgelib`
- PyPI: `forgelib`

All other entries above remain reservation scaffolds until their implementations pass
the same conformance contract as the active bindings. Their package checks stay in CI,
but publishing them is disabled by default in the release workflow.
