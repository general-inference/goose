//! Host process creation for the shared shell tool.
use async_trait::async_trait;
use std::{io, process::ExitStatus};
use tokio::io::AsyncRead;

/// The shell request before any host process or environment probing occurs.
pub struct ShellCommand<'a> {
    pub command: &'a str,
    pub working_dir: Option<&'a std::path::Path>,
    pub session_id: Option<&'a str>,
}

pub type ProcessOutput = Box<dyn AsyncRead + Send + Unpin>;

/// A process and its descendants. Dropping an active process must stop it.
/// Implementations must reap terminated children and avoid killing unrelated processes.
#[async_trait]
pub trait RunningShell: Send {
    fn take_stdout(&mut self) -> Option<ProcessOutput>;
    fn take_stderr(&mut self) -> Option<ProcessOutput>;
    async fn wait(&mut self) -> io::Result<ExitStatus>;
    async fn kill(&mut self) -> io::Result<()>;
}

/// Applies the host's sandbox and environment policy when starting a command.
/// Cancellation may drop `spawn`; implementations must not leave an orphan process.
#[async_trait]
pub trait ShellExecutor: Send + Sync {
    async fn spawn(&self, command: ShellCommand<'_>) -> io::Result<Box<dyn RunningShell>>;
}

pub struct NativeShellExecutor;
#[async_trait]
impl ShellExecutor for NativeShellExecutor {
    async fn spawn(&self, request: ShellCommand<'_>) -> io::Result<Box<dyn RunningShell>> {
        let mut command = crate::shell::build_shell_command(
            request.command,
            request.working_dir,
            request.session_id,
        )
        .await;
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        command.stdin(std::process::Stdio::null());
        Ok(Box::new(crate::shell_process::ShellProcess::spawn(
            &mut command,
        )?))
    }
}
