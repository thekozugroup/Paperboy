use anyhow::{Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

/// Commands never pass through a shell. Timeouts terminate the entire process group.
pub async fn run(program: &str, args: &[String], seconds: u64) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.kill_on_drop(true).process_group(0);
    let child = command.spawn()?;
    let pid = child.id().map(|pid| Pid::from_raw(pid as i32));
    match timeout(Duration::from_secs(seconds), child.wait_with_output()).await {
        Ok(result) => {
            let result = result?;
            if !result.status.success() {
                bail!("The processing tool could not open this file.");
            }
            Ok(result.stdout)
        }
        Err(_) => {
            if let Some(pid) = pid {
                let _ = killpg(pid, Signal::SIGKILL);
            }
            bail!("Processing took too long. Try exporting the file as PDF.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_deadline_is_enforced() {
        let start = std::time::Instant::now();
        assert!(run("sleep", &["30".into()], 1).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
