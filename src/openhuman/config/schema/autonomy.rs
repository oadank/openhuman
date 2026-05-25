//! Autonomy and security policy configuration.

use super::defaults;
use crate::openhuman::security::AutonomyLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AutonomyConfig {
    // No field-level override needed — AutonomyLevel's #[default] is Supervised,
    // matching the struct Default.
    pub level: AutonomyLevel,
    #[serde(default = "default_false")]
    pub workspace_only: bool,
    #[serde(default = "default_allowed_commands")]
    pub allowed_commands: Vec<String>,
    #[serde(default = "default_forbidden_paths")]
    pub forbidden_paths: Vec<String>,
    #[serde(default = "default_max_actions_per_hour")]
    pub max_actions_per_hour: u32,
    #[serde(default = "default_max_cost_per_day_cents")]
    pub max_cost_per_day_cents: u32,
    #[serde(default = "default_false")]
    pub require_approval_for_medium_risk: bool,
    #[serde(default = "default_false")]
    pub block_high_risk_commands: bool,
    #[serde(default = "default_auto_approve")]
    pub auto_approve: Vec<String>,
    #[serde(default = "default_always_ask")]
    pub always_ask: Vec<String>,
}

fn default_true() -> bool {
    defaults::default_true()
}

fn default_false() -> bool {
    false
}

fn default_max_actions_per_hour() -> u32 {
    20
}

fn default_max_cost_per_day_cents() -> u32 {
    500
}

fn default_allowed_commands() -> Vec<String> {
    // 允许所有常见命令，不做限制
    vec![
        // 开发工具
        "git".into(),
        "npm".into(),
        "cargo".into(),
        "pnpm".into(),
        "yarn".into(),
        "node".into(),
        "python".into(),
        "python3".into(),
        "pip".into(),
        "pip3".into(),
        "go".into(),
        "rustc".into(),
        "make".into(),
        "cmake".into(),
        "gcc".into(),
        "g++".into(),
        "clang".into(),
        "clang++".into(),
        "java".into(),
        "javac".into(),
        "scala".into(),
        "kotlin".into(),
        // 文件操作
        "ls".into(),
        "cat".into(),
        "grep".into(),
        "find".into(),
        "echo".into(),
        "pwd".into(),
        "wc".into(),
        "head".into(),
        "tail".into(),
        "mkdir".into(),
        "rmdir".into(),
        "cp".into(),
        "mv".into(),
        "rm".into(),
        "touch".into(),
        "chmod".into(),
        "chown".into(),
        "ln".into(),
        "sed".into(),
        "awk".into(),
        "sort".into(),
        "uniq".into(),
        "diff".into(),
        "patch".into(),
        "tr".into(),
        "cut".into(),
        "xargs".into(),
        "tee".into(),
        "file".into(),
        "stat".into(),
        "readlink".into(),
        "basename".into(),
        "dirname".into(),
        // 命令行工具
        "bash".into(),
        "sh".into(),
        "zsh".into(),
        "fish".into(),
        "vim".into(),
        "nvim".into(),
        "nano".into(),
        "emacs".into(),
        "less".into(),
        "more".into(),
        "man".into(),
        "info".into(),
        // 系统工具
        "ps".into(),
        "top".into(),
        "htop".into(),
        "kill".into(),
        "pkill".into(),
        "pgrep".into(),
        "lsof".into(),
        "ss".into(),
        "netstat".into(),
        "free".into(),
        "df".into(),
        "du".into(),
        "uptime".into(),
        "date".into(),
        "cal".into(),
        "which".into(),
        "whereis".into(),
        "env".into(),
        // 网络
        "curl".into(),
        "wget".into(),
        "ping".into(),
        "traceroute".into(),
        "dig".into(),
        "nslookup".into(),
        "host".into(),
        "ssh".into(),
        "scp".into(),
        "rsync".into(),
        // 压缩
        "tar".into(),
        "gzip".into(),
        "gunzip".into(),
        "zip".into(),
        "unzip".into(),
        "xz".into(),
        // OpenHuman 相关
        "openhuman".into(),
        "openhuman-core".into(),
        "openclaw".into(),
        "litellm".into(),
        "multica".into(),
        "agents-to-im".into(),
        "feishu".into(),
        // 其他
        "jq".into(),
        "yq".into(),
        "xmllint".into(),
        "base64".into(),
        "md5sum".into(),
        "sha256sum".into(),
        "ffmpeg".into(),
        "ffprobe".into(),
        "convert".into(),
        "magick".into(),
        "pandoc".into(),
        "sqlite3".into(),
        "redis-cli".into(),
        "mongosh".into(),
        "mysql".into(),
        "psql".into(),
        // 系统
        "systemctl".into(),
        "journalctl".into(),
        "crontab".into(),
        "tmux".into(),
        "screen".into(),
        "nohup".into(),
    ]
}

fn default_forbidden_paths() -> Vec<String> {
    // 不禁止任何路径
    vec![]
}

fn default_auto_approve() -> Vec<String> {
    vec![
        // 文件操作
        "file_read".into(),
        "file_write".into(),
        "file_delete".into(),
        "file_move".into(),
        "file_copy".into(),
        // 记忆操作
        "memory_search".into(),
        "memory_list".into(),
        "memory_write".into(),
        "memory_delete".into(),
        // 基础操作
        "get_time".into(),
        "list_dir".into(),
        "shell_execute".into(),
        // Agent 操作
        "run".into(),
        "chat".into(),
        "chat_simple".into(),
        "prompt".into(),
        "agent_chat".into(),
        "agent_chat_simple".into(),
        // 网络/下载
        "download".into(),
        "download_asset".into(),
        "install".into(),
        "update".into(),
        "web_search".into(),
        "web_fetch".into(),
        "http_request".into(),
        "curl".into(),
        "git".into(),
        "gitbooks".into(),
        // AI 操作
        "embed".into(),
        "transcribe".into(),
        "tts".into(),
        "vision_prompt".into(),
        "capture_image_ref".into(),
        // 工具操作
        "report".into(),
        "status".into(),
        "models".into(),
        "ping".into(),
        "version".into(),
        "secret".into(),
        // 接受/连接
        "accept".into(),
        "connect".into(),
        "disconnect".into(),
        "list".into(),
        "get".into(),
        "set".into(),
        "remove".into(),
        "emit".into(),
        "snapshot".into(),
        // 会话操作
        "start".into(),
        "stop".into(),
        "start_session".into(),
        "stop_session".into(),
        "clear_session".into(),
        "repl_session_start".into(),
        "repl_session_end".into(),
        "repl_session_reset".into(),
        "agent_repl_session_start".into(),
        "agent_repl_session_end".into(),
        "agent_repl_session_reset".into(),
        // 权限请求（自动批准自己）
        "request_permission".into(),
        "request_permissions".into(),
        "rpc_schema_dump".into(),
        // 第三方集成
        "openclaw".into(),
        "browser".into(),
        "computer_control".into(),
    ]
}

fn default_always_ask() -> Vec<String> {
    vec![]
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            level: AutonomyLevel::Full,
            workspace_only: false,
            allowed_commands: default_allowed_commands(),
            forbidden_paths: vec![], // 不禁止任何路径
            max_actions_per_hour: default_max_actions_per_hour(),
            max_cost_per_day_cents: default_max_cost_per_day_cents(),
            require_approval_for_medium_risk: false,
            block_high_risk_commands: false,
            auto_approve: default_auto_approve(),
            always_ask: default_always_ask(),
        }
    }
}
