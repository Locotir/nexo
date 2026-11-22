use super::*;
use nexo_core::jobs::{JobInfo, JobKind, JobStatus, JobTail};

#[test]
fn tails_before_monitor_ids_survive_later_job_snapshots() {
    let (mut a, _) = bare();
    a.handle_agent(AgentEvent::JobTails {
        tails: vec![JobTail { id: 4, text: "retained".into(), ended_ago_ms: None }],
    });
    a.handle_agent(AgentEvent::Jobs {
        jobs: vec![JobInfo {
            id: 4,
            kind: JobKind::Shell,
            label: "work".into(),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 1,
            held: false,
        }],
    });
    a.handle_agent(AgentEvent::Monitors { ids: vec![4], states: vec![] });
    assert_eq!(a.monitors.len(), 1);
    let view = crate::jobs_view::JobsView::monitors(a.monitors.clone(), a.monitor_tails.clone());
    let text = view.lines(100, &crate::theme::Theme::dark()).0.iter().map(|l| l.to_string()).collect::<String>();
    assert!(text.contains("retained"), "{text}");
}

#[test]
fn monitor_state_updates_open_list_and_detail_independently_of_job() {
    use nexo_core::agent::MonitorStatus::*;
    let (mut a, _) = bare();
    a.handle_agent(AgentEvent::Jobs {
        jobs: vec![JobInfo {
            id: 4,
            kind: JobKind::Shell,
            label: "work".into(),
            status: JobStatus::Running,
            elapsed_ms: 0,
            lines: 0,
            held: false,
        }],
    });
    a.handle_agent(AgentEvent::Monitors { ids: vec![4], states: vec![(4, Listening)] });
    let mut view = crate::jobs_view::JobsView::monitors(a.monitors.clone(), vec![]);
    view.set_monitor_states(a.monitor_states.clone());
    a.modal = Some(crate::modal::Modal::Jobs(Box::new(view)));
    for state in [Listening, Paused, Closing] {
        a.handle_agent(AgentEvent::Monitors { ids: vec![4], states: vec![(4, state)] });
        let Some(crate::modal::Modal::Jobs(view)) = &mut a.modal else { panic!("monitor view") };
        let word = match state {
            Listening => "listening",
            Paused => "paused",
            Closing => "closing",
        };
        let text = view.lines(100, &crate::theme::Theme::dark()).0.iter().map(|l| l.to_string()).collect::<String>();
        assert!(text.contains(word), "{text}");
        view.detail = true;
        let text = view.lines(100, &crate::theme::Theme::dark()).0.iter().map(|l| l.to_string()).collect::<String>();
        assert!(text.contains(&format!("monitor: {word}")) && text.contains("job: running"), "{text}");
        view.detail = false;
    }
}
