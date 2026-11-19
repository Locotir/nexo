//! The step count of a turn counts only the main agent's model calls.

use super::*;

#[test]
fn only_the_main_agents_calls_are_counted_as_steps() {
    let (mut a, _rx) = bare();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    // one call of the main agent, with the usage it reported
    a.handle_agent(AgentEvent::Usage {
        turn: Usage { input: 100, output: 10, ..Default::default() },
        call: Usage { input: 100, output: 10, ..Default::default() },
        session: Usage { input: 100, output: 10, ..Default::default() },
        cost_usd: 0.0,
    });
    // three events of sub-agents, the reader or compaction: they spend, but carry no call usage
    for i in 1..=3u64 {
        a.handle_agent(AgentEvent::Usage {
            turn: Usage { input: 100 + 50 * i, output: 10 + 5 * i, ..Default::default() },
            call: Usage::default(),
            session: Usage { input: 100 + 50 * i, output: 10 + 5 * i, ..Default::default() },
            cost_usd: 0.0,
        });
    }
    assert_eq!(a.turn_tally.steps, 1, "the card counts the main agent's calls only");
    assert_eq!(a.turn_tally.steps as usize, a.call_inputs.len(), "the card and the title bar agree");
    assert_eq!(a.turn_usage.input, 100 + 50 * 3, "the spend of all four events reaches the turn");
    assert_eq!(a.turn_usage.output, 10 + 5 * 3);
}
