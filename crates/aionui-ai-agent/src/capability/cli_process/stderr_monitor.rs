use aionui_common::AppError;
use tracing::{debug, error};

/// Send SIGKILL to a process by PID.
///
/// Uses the system `kill` command to avoid a `libc` dependency.
/// If the process has already exited, this is a no-op.
pub(super) fn force_kill(pid: u32) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let result = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                debug!(pid, "SIGKILL sent successfully");
                Ok(())
            }
            Ok(_output) => {
                // Non-zero exit likely means process already exited — acceptable
                debug!(pid, "Process already exited before SIGKILL");
                Ok(())
            }
            Err(e) => {
                error!(pid, error = %e, "Failed to execute kill command");
                Err(AppError::Internal(format!("Failed to kill process {pid}: {e}")))
            }
        }
    }
    #[cfg(not(unix))]
    {
        Err(AppError::Internal(format!(
            "Force kill not supported on this platform for pid {pid}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::CliAgentProcess;
    use super::super::tests::simple_script_config;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn stderr_captured_in_buffer() {
        let config = simple_script_config("echo 'error line 1' >&2 && echo 'error line 2' >&2");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        timeout(Duration::from_secs(5), proc.wait_for_exit()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let stderr = proc.take_stderr().await;
        assert!(stderr.contains("error line 1"), "stderr: {stderr}");
        assert!(stderr.contains("error line 2"), "stderr: {stderr}");
    }

    #[tokio::test]
    async fn take_stderr_is_consuming() {
        let config = simple_script_config("echo 'hello' >&2");
        let proc = CliAgentProcess::spawn(config).await.unwrap();

        timeout(Duration::from_secs(5), proc.wait_for_exit()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let first = proc.take_stderr().await;
        assert!(!first.is_empty());

        let second = proc.take_stderr().await;
        assert!(second.is_empty(), "Second take should be empty");
    }
}
