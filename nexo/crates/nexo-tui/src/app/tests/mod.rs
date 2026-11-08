//! Unit tests of the application state: shared helpers here, the cases in the submodules.

use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

fn app() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let opts = TuiOptions {
        cwd: PathBuf::from("."),
        provider: "mock".into(),
        model: "m".into(),
        models: vec![("mock".into(), "m".into()), ("mock".into(), "n".into())],
        providers: vec![(
            "ollama".into(),
            nexo_core::ProviderConfig::new("openai-chat", "http://localhost:11434/v1", None),
        )],
        theme: "dark".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 3,
        group_tools: true,
        complete_words: true,
        suggest: false,
        keys: Default::default(),
        notify_after: 15,
        animations: true,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: None,
        sessions: vec![],
        skills: vec![],
    };
    let mut a = App::new(opts, tx);
    a.root = false;
    (a, rx)
}

/// An app whose transcript starts empty: tests that count blocks from the first.
fn bare() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, rx) = app();
    a.blocks.clear();
    a.cache.clear();
    a.looks.clear();
    (a, rx)
}

// ------------------------------------------------------------ groups of calls

fn running(id: &str) -> Block {
    let mut b = card(id, 1);
    if let Block::Tool { status, output, started, .. } = &mut b {
        *status = ToolStatus::Running;
        *output = None;
        *started = Some(Instant::now());
    }
    b
}

fn thinking() -> Block {
    Block::Reasoning { text: "hm".into(), streaming: false, started: None, elapsed_ms: Some(5), folded: true }
}

fn head(a: &mut App, i: usize) -> String {
    a.lines(i, 80).first().map(crate::render::markdown::text_of).unwrap_or_default()
}

fn groups(a: &App) -> usize {
    a.blocks.iter().filter(|b| matches!(b, Block::Group { .. })).count()
}

/// Two providers list a model of the same name; the session runs on the first.
fn two_luna() -> (App, mpsc::UnboundedReceiver<AgentCommand>) {
    let (mut a, rx) = app();
    a.provider = "refuseless".into();
    a.model = "deepseek-chat".into();
    a.opts.providers.push(("codex".into(), nexo_core::ProviderConfig::new("openai-chat", "http://x/v1", None)));
    a.opts.models = vec![
        ("refuseless".into(), "deepseek-chat".into()),
        ("refuseless".into(), "luna".into()),
        ("codex".into(), "luna".into()),
    ];
    (a, rx)
}

fn title(a: &App) -> String {
    crate::ui::title_segments(a, 400).iter().flatten().map(|s| s.text.clone()).collect::<String>()
}

fn card(id: &str, lines: usize) -> Block {
    let out = ToolOutput::ok((1..=lines).map(|i| format!("{id} line {i}")).collect::<Vec<_>>().join("\n"));
    Block::Tool {
        id: id.into(),
        name: "bash".into(),
        summary: format!("bash: run {id}"),
        args: serde_json::Value::Null,
        status: ToolStatus::Done,
        progress: vec![],
        output: Some(out),
        started: None,
        elapsed_ms: None,
        folded: true,
    }
}

fn question(id: &str, multi: bool, plan: Option<&str>) -> nexo_core::permission::Question {
    let opt = |l: &str| nexo_core::permission::QuestionOption { label: l.into(), description: format!("about {l}") };
    nexo_core::permission::Question {
        id: id.into(),
        header: "Pick".into(),
        question: "Which one?".into(),
        options: vec![opt("red"), opt("green"), opt("blue")],
        multi,
        plan: plan.map(str::to_string),
    }
}

fn press(a: &mut App, code: KeyCode) {
    crate::event::key(a, KeyEvent::new(code, KeyModifiers::NONE));
}

fn answer_of(rx: &mut mpsc::UnboundedReceiver<AgentCommand>) -> Option<(String, Option<String>)> {
    std::iter::from_fn(|| rx.try_recv().ok()).find_map(|c| match c {
        AgentCommand::Answer { id, answer } => Some((id, answer)),
        _ => None,
    })
}

fn pointer(a: &mut App, kind: crossterm::event::MouseEventKind, x: u16, y: u16) {
    let m = crossterm::event::MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    crate::event::mouse(a, m);
}

fn frame(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::ui::draw(f, a)).unwrap();
    crate::ui::rows_of(term.backend().buffer())
}

fn long_answer(a: &mut App, from: usize) {
    let text: Vec<String> = (from..from + 60).map(|i| format!("line {i}")).collect();
    a.push(Block::Assistant { text: text.join("\n\n"), streaming: false, at: None, started: None, took_ms: None });
}

fn drain(rx: &mut mpsc::UnboundedReceiver<AgentCommand>) -> Vec<AgentCommand> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn queue_two(a: &mut App, rx: &mut mpsc::UnboundedReceiver<AgentCommand>) {
    a.busy = true;
    for t in ["first request", "second request"] {
        a.input.set_text(t);
        a.submit();
    }
    a.status = None;
    drain(rx);
}

fn lit_cells(a: &mut App, r: Rect) -> bool {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 20)).unwrap();
    term.draw(|f| crate::ui::draw(f, a)).unwrap();
    let buf = term.backend().buffer();
    (r.x + 1..r.x + r.width).all(|x| buf[(x, r.y)].modifier.contains(ratatui::style::Modifier::UNDERLINED))
}

fn fx_at(a: &mut App, t0: Instant, ms: u64) {
    a.fx.clock = Some(t0 + std::time::Duration::from_millis(ms));
}

fn buf_of(a: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::ui::draw(f, a)).unwrap();
    term.backend().buffer().clone()
}

fn screen(a: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    crate::draw_frame(&mut t, a).unwrap();
    crate::ui::rows_of(t.backend().buffer())
}

fn cached(a: &App) -> usize {
    a.cache.iter().filter(|c| matches!(c, Some((_, _, Some(_), _)))).count()
}

fn docs_server() -> nexo_core::agent::McpServerInfo {
    use nexo_core::agent::{McpArgInfo, McpPromptInfo, McpServerInfo, McpStatus};
    McpServerInfo {
        name: "docs".into(),
        transport: "http".into(),
        status: McpStatus::Connected,
        tools: 4,
        resources: vec!["file:///notes.md".into(), "db://users".into()],
        templates: vec![],
        prompts: vec![McpPromptInfo {
            name: "review".into(),
            description: "Review a file".into(),
            args: vec![McpArgInfo { name: "file".into(), required: true }],
        }],
    }
}

mod agent_a;
mod agent_b;
mod agent_c;
