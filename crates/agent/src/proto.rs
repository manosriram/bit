pub use llm::Usage;

#[derive(Debug, PartialEq)]
pub enum Command {
    Submit(String),
    Approve { id: Box<str>, decision: Decision },
    Cancel,
    SetMode(Mode),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    Yes,
    No,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Ask,
    Auto,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// A user message; sent only when replaying a resumed session.
    User(Box<str>),
    Text(Box<str>),
    ToolStart {
        id: Box<str>,
        name: Box<str>,
        summary: Box<str>,
    },
    Approval {
        id: Box<str>,
        name: Box<str>,
        summary: Box<str>,
        detail: Box<str>,
    },
    ToolEnd {
        id: Box<str>,
        ok: bool,
        preview: Box<str>,
    },
    Usage(Usage),
    Notice(Box<str>),
    TurnEnd(StopReason),
}

#[derive(Debug, PartialEq)]
pub enum StopReason {
    Done,
    Cancelled,
    Error(Box<str>),
}
