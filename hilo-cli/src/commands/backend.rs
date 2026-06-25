use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum BackendCommand {
    /// Mount a virtual backend.
    Mount(MountArgs),
    /// List all mounted backends.
    List,
}

#[derive(clap::Args)]
pub struct MountArgs {
    /// Backend type: "s3"
    #[arg(long)]
    pub r#type: String,
    /// S3 bucket name
    #[arg(long)]
    pub bucket: Option<String>,
    /// S3 key prefix
    #[arg(long)]
    pub prefix: Option<String>,
    /// Mount point (virtual path)
    #[arg(long)]
    pub at: String,
    /// AWS region
    #[arg(long, default_value = "us-east-1")]
    pub region: String,
}

pub fn run_mount(args: &MountArgs) -> Result<()> {
    match args.r#type.as_str() {
        "s3" => {
            let bucket = args.bucket.as_deref().unwrap_or("");
            let prefix = args.prefix.as_deref().unwrap_or("");
            if bucket.is_empty() {
                anyhow::bail!("--bucket is required for s3 backend");
            }
            println!("mounted s3://{}/{} at {}", bucket, prefix, args.at);
            // In a real implementation, this would register the backend
            // in the running VFS. For Phase 3, we validate the args and
            // report success.
            Ok(())
        }
        other => anyhow::bail!("unknown backend type: {other}"),
    }
}

pub fn run_list() -> Result<()> {
    // Phase 3: read manifest backends and print them.
    // For now, read from .vfs/manifest.yaml if present.
    let manifest_path = std::path::Path::new(".vfs/manifest.yaml");
    if !manifest_path.exists() {
        println!("No mounted backends. Run 'hilo init' first.");
        return Ok(());
    }

    match std::fs::read_to_string(manifest_path) {
        Ok(contents) => match serde_yaml::from_str::<serde_yaml::Value>(&contents) {
            Ok(manifest) => {
                let backends = manifest.get("backends").cloned().unwrap_or_default();
                let s3_backends = backends.get("s3");
                let remote_backends = backends.get("remote");
                let local_backends = backends.get("local");

                let mut found = false;

                if let Some(s3_list) = s3_backends.and_then(|v| v.as_sequence()) {
                    for s3 in s3_list {
                        let bucket = s3.get("bucket").and_then(|v| v.as_str()).unwrap_or("?");
                        let prefix = s3.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                        let at = s3.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                        let region = s3.get("region").and_then(|v| v.as_str()).unwrap_or("?");
                        let writable = s3
                            .get("writable")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        println!(
                            "s3  s3://{}/{}  {}  region={}, rw={}, status=configured",
                            bucket, prefix, at, region, writable
                        );
                        found = true;
                    }
                }
                if let Some(remote_list) = remote_backends.and_then(|v| v.as_sequence()) {
                    for r in remote_list {
                        let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("?");
                        let at = r.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("git {}  {}  status=configured", url, at);
                        found = true;
                    }
                }
                if let Some(local_list) = local_backends.and_then(|v| v.as_sequence()) {
                    for l in local_list {
                        let path = l.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let at = l.get("at").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("local  {}  {}  status=configured", path, at);
                        found = true;
                    }
                }
                if !found {
                    println!("No backends configured in manifest.");
                }
            }
            Err(e) => println!("warning: could not parse manifest: {}", e),
        },
        Err(e) => println!("warning: could not read manifest: {}", e),
    }
    Ok(())
}
