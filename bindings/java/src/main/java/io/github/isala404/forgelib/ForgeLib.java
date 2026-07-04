package io.github.isala404.forgelib;

/** Metadata for the future first-party Forge JVM binding. */
public final class ForgeLib {
    public static final String STATUS = "metadata-only";
    public static final String REPOSITORY = "https://github.com/isala404/forge";

    private ForgeLib() {
    }

    public static String notice() {
        return "Forge JVM bindings are planned. Use Rust, Node.js, or Python today.";
    }
}
