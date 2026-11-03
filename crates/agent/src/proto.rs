pub use llm::Usage;

pub enum Command {
    Submit(String),
    Approve {
        id: Box<str>,
        decision: Decision,
    },
    /// The user's reply to `Event::Ask`.
    Answer {
        id: Box<str>,
        text: String,
    },
    Cancel,
    SetMode(Mode),
    /// Sent only while idle.
    SetModel {
        provider: Box<str>,
        model: Box<str>,
    },
    /// Drops the history. Sent only while idle.
    Clear,
    /// Summarizes the history now. Runs like a turn and ends with `TurnEnd`.
    Compact,
    /// Asks which provider to sign in to, then signs in. Runs like a turn.
    Login,
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
        /// What the call changes, shown once it succeeds: `-`/`+` lines for `edit`, else empty.
        diff: Box<str>,
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
        /// The full result, as the model got it.
        output: Box<str>,
    },
    /// The model asks the user to choose; answered with `Command::Answer`.
    Ask {
        id: Box<str>,
        question: Box<str>,
        options: Vec<Box<str>>,
    },
    /// The tokens of the last reply.
    Usage(Usage),
    Notice(Box<str>),
    /// The provider now has a key, from `/login`.
    SignedIn(Box<str>),
    TurnEnd(StopReason),
}

#[derive(Debug, PartialEq)]
pub enum StopReason {
    Done,
    Cancelled,
    Error(Box<str>),
}
