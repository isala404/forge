namespace ForgeLib
{
    /// <summary>Metadata for the future first-party Forge .NET binding.</summary>
    public static class ForgeLibInfo
    {
        /// <summary>Current package status.</summary>
        public const string Status = "metadata-only";

        /// <summary>Forge source repository.</summary>
        public const string Repository = "https://github.com/isala404/forge";

        /// <summary>Human-readable placeholder package notice.</summary>
        public static string Notice =>
            "Forge .NET bindings are planned. Use Rust, Node.js, or Python today.";
    }
}
