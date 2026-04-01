fn main() {
    // Bake the shared Turso P2P index credentials into the binary at build time.
    // TURSO_TOKEN is set in GitHub Actions repository secrets.
    // Falls back to empty string for local builds without the secret.
    let token = std::env::var("TURSO_TOKEN").unwrap_or_default();
    println!("cargo:rustc-env=DJA_TURSO_TOKEN={token}");
    println!(
        "cargo:rustc-env=DJA_TURSO_URL=libsql://dja-shared-index-mesmes.aws-eu-west-1.turso.io"
    );
    // Re-run if the env var changes
    println!("cargo:rerun-if-env-changed=TURSO_TOKEN");
}
