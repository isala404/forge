Pod::Spec.new do |spec|
  spec.name = "ForgeLib"
  spec.version = "0.0.1"
  spec.summary = "Official Swift package for Forge."
  spec.description = "Metadata release for the future first-party Swift binding for Forge, the standard library for agent-built SaaS."
  spec.homepage = "https://github.com/isala404/forge"
  spec.license = { type: "MIT", file: "LICENSE" }
  spec.author = { "Isala Piyarisi" => "mail@isala.me" }
  spec.source = { git: "https://github.com/isala404/forge.git", tag: "forgelib-v0.0.1" }
  spec.source_files = "bindings/swift/Sources/ForgeLib/**/*.swift"
  spec.swift_version = "5.9"
  spec.ios.deployment_target = "13.0"
  spec.macos.deployment_target = "10.15"
end
