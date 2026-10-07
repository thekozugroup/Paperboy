use anyhow::{Result, bail};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

pub async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Signal handler");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
}

/// Commands never pass through a shell. Timeouts terminate the entire process group.
pub async fn run(program: &str, args: &[String], seconds: u64) -> Result<Vec<u8>> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.kill_on_drop(true).process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id().map(|pid| Pid::from_raw(pid as i32));
    let stdout = child.stdout.take().expect("Piped stdout");
    let operation = async {
        let mut output = Vec::new();
        stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut output)
            .await?;
        if output.len() > 1024 * 1024 {
            bail!("The processing tool produced too much output.");
        }
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((output, status))
    };
    match timeout(Duration::from_secs(seconds), operation).await {
        Ok(Ok((output, status))) => {
            if !status.success() {
                bail!("The processing tool could not open this file.");
            }
            Ok(output)
        }
        result => {
            if let Some(pid) = pid {
                let _ = killpg(pid, Signal::SIGKILL);
            }
            let _ = child.wait().await;
            match result {
                Ok(Err(error)) => Err(error),
                _ => bail!("Processing took too long. Try exporting the file as PDF."),
            }
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
    #[tokio::test]
    async fn command_output_cannot_grow_without_bound() {
        let start = std::time::Instant::now();
        assert!(run("yes", &["fixture".into()], 5).await.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
