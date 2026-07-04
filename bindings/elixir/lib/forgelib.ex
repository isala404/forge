defmodule Forgelib do
  @moduledoc """
  Metadata for the future first-party Forge Elixir binding.
  """

  @status "metadata-only"
  @repository "https://github.com/isala404/forge"

  def status, do: @status
  def repository, do: @repository

  def notice do
    "Forge Elixir bindings are planned. Use Rust, Node.js, or Python today."
  end
end
