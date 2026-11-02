pub use llm::Usage;

#[derive(Debug, PartialEq)]
pub enum Command {
    Submit(String),
    Cancel,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Text(Box<str>),
    Usage(Usage),
    TurnEnd(StopReason),
}

#[derive(Debug, PartialEq)]
pub enum StopReason {
    Done,
    Cancelled,
    Error(Box<str>),
}
