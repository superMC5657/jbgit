use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellPos {
    pub col: u16,
    pub row: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ColorPair {
    pub fg: String,
    pub bg: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub id: u64,
    pub action: PilotAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextTarget {
    pub needle: String,
    #[serde(default)]
    pub row: Option<u16>,
    #[serde(default)]
    pub index: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTarget {
    #[serde(flatten)]
    pub pos: CellPos,
    pub start_col: u16,
    pub end_col: u16,
    pub match_index: usize,
    pub match_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PilotAction {
    Screen {
        settle_ms: u64,
    },
    Keys {
        keys: Vec<String>,
        settle_ms: u64,
    },
    TypeText {
        text: String,
        settle_ms: u64,
    },
    Click {
        #[serde(flatten)]
        pos: CellPos,
        button: MouseBtn,
        #[serde(default)]
        modifiers: MouseModifiers,
        settle_ms: u64,
    },
    ClickText {
        #[serde(flatten)]
        target: TextTarget,
        button: MouseBtn,
        #[serde(default)]
        modifiers: MouseModifiers,
        settle_ms: u64,
    },
    Scroll {
        up: bool,
        #[serde(flatten)]
        pos: CellPos,
        steps: u16,
        settle_ms: u64,
    },
    Hover {
        #[serde(flatten)]
        pos: CellPos,
        settle_ms: u64,
    },
    HoverText {
        #[serde(flatten)]
        target: TextTarget,
        settle_ms: u64,
    },
    Clipboard,
    Resize {
        cols: u16,
        rows: u16,
        settle_ms: u64,
    },
    FindText {
        needle: String,
    },
    InspectRow {
        row: u16,
    },
    PaletteSummary,
    WaitIdle {
        quiet_ms: u64,
        timeout_ms: u64,
    },
    Stop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseBtn {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MouseModifiers {
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub id: u64,
    pub ok: bool,
    pub error: Option<String>,
    pub alive: bool,
    pub exit_code: Option<i32>,
    pub cols: u16,
    pub rows: u16,
    pub cursor: CellPos,
    pub screen: Option<String>,
    pub matches: Vec<TextMatch>,
    pub row_spans: Vec<RowSpanInfo>,
    pub palette: Vec<ColorCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ResolvedTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMatch {
    #[serde(flatten)]
    pub pos: CellPos,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_col: Option<u16>,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RowSpanInfo {
    pub start_col: u16,
    pub end_col: u16,
    pub text: String,
    #[serde(flatten)]
    pub colors: ColorPair,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorCount {
    #[serde(flatten)]
    pub colors: ColorPair,
    pub cells: usize,
}
