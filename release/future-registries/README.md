# Future registry lanes

Forge 1.0 ships Rust, Node.js, and Python packages. The release workflow also has
explicit no-op lanes for ecosystems we may support later, so adding a new binding is
mechanical instead of inventing release plumbing from scratch.

| Registry | Expected package marker | Status for 1.0 |
| --- | --- | --- |
| pub.dev | `bindings/dart/pubspec.yaml` | no Dart package yet |
| Maven Central | `bindings/jvm/build.gradle.kts` | no JVM package yet |
| NuGet | `bindings/dotnet/*.csproj` | no .NET package yet |
| RubyGems | `bindings/ruby/*.gemspec` | no Ruby package yet |
| Go modules | `bindings/go/go.mod` | no Go package yet |

When one of those markers exists, replace the corresponding no-op in
`.github/workflows/release.yml` with the real build and publish command for that
ecosystem.
