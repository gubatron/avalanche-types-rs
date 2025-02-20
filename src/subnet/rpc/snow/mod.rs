/// ref. https://pkg.go.dev/github.com/ava-labs/avalanchego/snow#State
#[derive(PartialEq, Eq)]
pub enum State {
    Initializing = 0,
    StateSyncing = 1,
    Bootstrapping = 2,
    NormalOp = 3,
}

impl State {
    /// ref. https://pkg.go.dev/github.com/ava-labs/avalanchego/snow#State.String
    pub fn as_str(&self) -> &str {
        match self {
            State::Initializing => "Initializing state",
            State::StateSyncing => "State syncing state",
            State::Bootstrapping => "Bootstrapping state",
            State::NormalOp => "Normal operations state",
        }
    }
}

impl TryFrom<u32> for State {
    type Error = ();

    fn try_from(kind: u32) -> std::result::Result<Self, Self::Error> {
        match kind {
            kind if kind == State::Initializing as u32 => Ok(State::Initializing),
            kind if kind == State::StateSyncing as u32 => Ok(State::StateSyncing),
            kind if kind == State::Bootstrapping as u32 => Ok(State::Bootstrapping),
            kind if kind == State::NormalOp as u32 => Ok(State::NormalOp),
            _ => Err(()),
        }
    }
}

#[test]
fn test_state() {
    let s = State::try_from(0).unwrap();
    assert!(matches!(s, State::Initializing));
    assert!(s.as_str() == "Initializing state");

    let s = State::try_from(1).unwrap();
    assert!(matches!(s, State::StateSyncing));
    assert!(s.as_str() == "State syncing state");

    let s = State::try_from(2).unwrap();
    assert!(matches!(s, State::Bootstrapping));
    assert!(s.as_str() == "Bootstrapping state");

    let s = State::try_from(3).unwrap();
    assert!(matches!(s, State::NormalOp));
    assert!(s.as_str() == "Normal operations state");
}
