defmodule Forgelib.MixProject do
  use Mix.Project

  def project do
    [
      app: :forgelib,
      version: "0.0.1",
      elixir: "~> 1.15",
      description: "Metadata release for the future first-party Elixir binding for Forge.",
      package: package()
    ]
  end

  def application do
    [extra_applications: [:logger]]
  end

  defp package do
    [
      licenses: ["MIT"],
      links: %{"GitHub" => "https://github.com/isala404/forge"}
    ]
  end
end
