use std::process::{Command, Stdio};

fn main() -> std::io::Result<()> {
    let mut p = Command::new("prodder")
        .stdout(Stdio::piped())
        .spawn()?;

    match std::env::var("DATADOG_API_KEY") {
        Ok(_) => {
            Command::new("vector")
                .args(["--config", "/etc/vector/vector.toml"])
                .stdin(p.stdout.take().expect("prodder stdout piped"))
                .spawn()?
                .wait()?;
            p.wait()?;
        }
        Err(_) => {
            eprintln!(
                "launcher: DATADOG_API_KEY not set; running prodder \
                 without vector sidecar"
            );
            p.wait()?;
        }
    }
    Ok(())
}
