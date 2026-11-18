use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// Unique identifier for an agent instance in the persistent pool.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub agent_id: String,
    pub workspace: Option<PathBuf>,
}

/// Status of a pooled persistent agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Busy,
    Benched,
    Terminated,
}

/// A persistent CLI agent subprocess running in stream-json mode.
pub struct PersistentAgent {
    pub key: PoolKey,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub status: AgentStatus,
    pub last_active: Instant,
    pub turn_count: usize,
    pub max_turns: usize,
}

impl PersistentAgent {
    pub async fn spawn(
        cmd_name: &str,
        args: &[&str],
        workspace: Option<&Path>,
        key: PoolKey,
    ) -> std::io::Result<Self> {
        let mut cmd = Command::new(cmd_name);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUNBUFFERED", "1");

        if let Some(ws) = workspace {
            cmd.current_dir(ws);
        }

        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to capture stdin")
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Failed to capture stdout")
        })?;

        Ok(Self {
            key,
            child,
            stdin,
            stdout: BufReader::new(stdout),
            status: AgentStatus::Idle,
            last_active: Instant::now(),
            turn_count: 0,
            max_turns: 50,
        })
    }

    pub async fn send_turn(
        &mut self,
        json_payload: &str,
    ) -> std::io::Result<Vec<String>> {
        self.status = AgentStatus::Busy;
        self.last_active = Instant::now();
        self.turn_count += 1;

        self.stdin.write_all(json_payload.as_bytes()).await?;
        if !json_payload.ends_with('\n') {
            self.stdin.write_all(b"\n").await?;
        }
        self.stdin.flush().await?;

        let mut output_lines = Vec::new();
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            let bytes_read = self.stdout.read_line(&mut line_buf).await?;
            if bytes_read == 0 {
                self.status = AgentStatus::Terminated;
                break;
            }

            let trimmed = line_buf.trim();
            if !trimmed.is_empty() {
                output_lines.push(trimmed.to_string());
                if trimmed.contains("\"type\":\"result\"")
                    || trimmed.contains("\"type\":\"turn_end\"")
                    || trimmed.contains("\"event\":\"turn_end\"")
                {
                    break;
                }
            }
        }

        self.status = AgentStatus::Idle;
        self.last_active = Instant::now();
        Ok(output_lines)
    }

    pub fn is_stale(&self, timeout: Duration) -> bool {
        self.last_active.elapsed() > timeout || self.turn_count >= self.max_turns
    }

    pub async fn kill(&mut self) -> std::io::Result<()> {
        self.status = AgentStatus::Terminated;
        self.child.kill().await
    }
}

/// Global manager for persistent CLI agent subprocesses.
pub struct AgentPool {
    agents: Arc<Mutex<HashMap<PoolKey, Arc<Mutex<PersistentAgent>>>>>,
    idle_timeout: Duration,
}

impl AgentPool {
    pub fn new(idle_timeout: Duration) -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
            idle_timeout,
        }
    }

    pub async fn get_or_spawn(
        &self,
        key: PoolKey,
        cmd_name: &str,
        args: &[&str],
        workspace: Option<&Path>,
    ) -> std::io::Result<Arc<Mutex<PersistentAgent>>> {
        let mut pool = self.agents.lock().await;

        if let Some(agent_arc) = pool.get(&key) {
            let mut agent = agent_arc.lock().await;
            if agent.status != AgentStatus::Terminated && !agent.is_stale(self.idle_timeout) {
                return Ok(agent_arc.clone());
            }
            let _ = agent.kill().await;
        }

        let new_agent = PersistentAgent::spawn(cmd_name, args, workspace, key.clone()).await?;
        let agent_arc = Arc::new(Mutex::new(new_agent));
        pool.insert(key, agent_arc.clone());
        Ok(agent_arc)
    }

    pub async fn reap_idle(&self) {
        let mut pool = self.agents.lock().await;
        let mut to_remove = Vec::new();

        for (k, agent_arc) in pool.iter() {
            let mut agent = agent_arc.lock().await;
            if agent.is_stale(self.idle_timeout) || agent.status == AgentStatus::Terminated {
                let _ = agent.kill().await;
                to_remove.push(k.clone());
            }
        }

        for k in to_remove {
            pool.remove(&k);
        }
    }

    pub async fn shutdown_all(&self) {
        let mut pool = self.agents.lock().await;
        for (_, agent_arc) in pool.drain() {
            let mut agent = agent_arc.lock().await;
            let _ = agent.kill().await;
        }
    }
}
