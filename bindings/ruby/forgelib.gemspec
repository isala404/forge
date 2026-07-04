Gem::Specification.new do |spec|
  spec.name = "forgelib"
  spec.version = "0.0.1"
  spec.authors = ["Isala Piyarisi"]
  spec.email = ["mail@isala.me"]

  spec.summary = "Official Ruby package for Forge"
  spec.description = "Metadata release for the future first-party Ruby binding for Forge, the standard library for agent-built SaaS."
  spec.homepage = "https://github.com/isala404/forge"
  spec.license = "MIT"
  spec.required_ruby_version = ">= 2.6"

  spec.metadata = {
    "homepage_uri" => spec.homepage,
    "source_code_uri" => "https://github.com/isala404/forge",
    "bug_tracker_uri" => "https://github.com/isala404/forge/issues"
  }

  spec.files = Dir["README.md", "lib/**/*.rb"]
  spec.require_paths = ["lib"]
end
