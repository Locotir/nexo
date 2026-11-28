//! A battery of short, realistic lines the owner might write, in English, Spanish or both, with what the
//! rule reader should make of each: a standing rule (or a change of one) or not. It measures misses and
//! false hits per group; the groups with a known gap are listed in `KNOWN`.

use crate::rules::rules_in;

/// (group, line, is it a rule)
const LINES: &[(&str, &str, bool)] = &[
    // true rules, English
    ("rule-en", "Always answer in English", true),
    ("rule-en", "from now on, commit messages in imperative mood", true),
    ("rule-en", "don't ever touch main without asking", true),
    ("rule-en", "Never push without asking me first", true),
    ("rule-en", "I want every test named as a sentence", true),
    ("rule-en", "going forward, keep the diffs small", true),
    ("rule-en", "from here on use tabs", true),
    ("rule-en", "Please never delete a branch on your own", true),
    ("rule-en", "do not run the full test suite, only the crate you touched", true),
    ("rule-en", "you must not edit generated files", true),
    ("rule-en", "As a rule, keep comments to why, not what", true),
    ("rule-en", "In future, ask before installing anything", true),
    ("rule-en", "Under no circumstances commit secrets", true),
    ("rule-en", "No emojis in the output, ever", true),
    ("rule-en", "Always run clippy before you say it is done", true),
    ("rule-en", "From now on you reply in Spanish to me and write code comments in English", true),
    ("rule-en", "make sure it never prints the token", true),
    ("rule-en", "I'd like you to stop summarizing at the end of every reply", true),
    ("rule-en", "Never, ever use sudo", true),
    ("rule-en", "keep answers under five lines", true),
    ("rule-en", "Whenever you finish a task, run the formatter", true),
    ("rule-en", "this has to stay offline, no network calls", true),
    // true rules, Spanish
    ("rule-es", "Siempre responde en inglés", true),
    ("rule-es", "nunca hagas push sin preguntarme", true),
    ("rule-es", "a partir de ahora los commits en inglés", true),
    ("rule-es", "de ahora en adelante no toques main", true),
    ("rule-es", "que no vuelva a pasar", true),
    ("rule-es", "no quiero emojis en nada", true),
    ("rule-es", "Jamás borres una rama sin avisar", true),
    ("rule-es", "Recuerda que los tests se llaman con frases", true),
    ("rule-es", "siempre en inglés", true),
    ("rule-es", "nunca uses sudo, usa pkexec", true),
    ("rule-es", "Desde ahora responde corto", true),
    ("rule-es", "En adelante pregunta antes de instalar nada", true),
    ("rule-es", "No vuelvas a tocar el fichero de configuración", true),
    ("rule-es", "Quiero que siempre pases clippy antes de terminar", true),
    ("rule-es", "Tienes que mantener la API pública estable", true),
    ("rule-es", "Procura no usar unwrap", true),
    ("rule-es", "Evita comentarios que repitan el código", true),
    ("rule-es", "ojo, nada de dependencias nuevas", true),
    ("rule-es", "Cada vez que cambies el esquema, sube la versión", true),
    ("rule-es", "Que quede claro: nunca se sube nada sin mi permiso", true),
    ("rule-es", "Y no te olvides, los mensajes de commit sin Co-Authored-By", true),
    // mixed languages
    ("rule-mix", "From now on, siempre responde en español", true),
    ("rule-mix", "a partir de ahora always run the tests antes de commit", true),
    ("rule-mix", "nunca hagas force push, ever", true),
    ("rule-mix", "No quiero que uses unwrap, use expect with a message", true),
    ("rule-mix", "don't hagas commit sin tests", true),
    ("rule-mix", "Ok, de ahora en adelante keep it short", true),
    // retractions and changes of mind
    ("retract", "ya no hace falta que pases clippy", true),
    ("retract", "olvida lo de los tabs", true),
    ("retract", "forget what I said about the changelog", true),
    ("retract", "now you can push", true),
    ("retract", "ahora sí puedes hacer push", true),
    ("retract", "ya puedes tocar main", true),
    ("retract", "no need to ask before installing anymore", true),
    ("retract", "you don't have to sign commits anymore", true),
    ("retract", "ignora lo que dije antes sobre los emojis", true),
    ("retract", "cambio de idea: usa tabs", true),
    ("retract", "I changed my mind, spaces are fine", true),
    ("retract", "Actually, you can use unwrap in tests", true),
    ("retract", "ya no tienes que preguntarme antes de hacer push", true),
    ("retract", "disregard what I said about the branch name", true),
    ("retract", "lo de antes ya no vale", true),
    ("retract", "no hace falta que respondas en inglés ya", true),
    ("retract", "Mejor al revés: responde en español", true),
    ("retract", "forget it, push whenever you want", true),
    ("retract", "ahora puedes instalar paquetes sin preguntar", true),
    ("retract", "that rule about tabs no longer applies", true),
    // two rules that contradict each other: both are kept, the later one wins in the block
    ("contradict", "Always answer in English", true),
    ("contradict", "Siempre responde en español", true),
    ("contradict", "Never push to main", true),
    ("contradict", "Push to main whenever you finish, don't ask", true),
    ("contradict", "Prefer tabs for indentation", true),
    ("contradict", "Use spaces, never tabs", true),
    ("contradict", "Always ask before installing anything", true),
    ("contradict", "Nunca me preguntes antes de instalar, hazlo", true),
    // bug reports that look like rules
    ("bug", "it always crashes when I open the settings", false),
    ("bug", "nunca carga la página", false),
    ("bug", "siempre falla el login en Windows", false),
    ("bug", "the build never finishes on CI", false),
    ("bug", "it never shows the progress bar", false),
    ("bug", "no funciona el botón de guardar", false),
    ("bug", "the app always freezes after the update", false),
    ("bug", "Sometimes it must be restarted twice to work", false),
    ("bug", "no me deja entrar nunca", false),
    ("bug", "siempre me sale un error 500", false),
    ("bug", "it doesn't work with the new config", false),
    ("bug", "the tests never pass on my machine", false),
    ("bug", "no compila desde ayer", false),
    ("bug", "it keeps showing the old value", false),
    ("bug", "I never get the notification", false),
    ("bug", "Always the same error: connection refused", false),
    ("bug", "the cache is always empty after restart", false),
    ("bug", "nunca me llega el correo de confirmación", false),
    ("bug", "it only works when I am logged out", false),
    ("bug", "no veo el botón en el móvil", false),
    // questions
    ("question", "do you always run the tests?", false),
    ("question", "¿siempre hace eso?", false),
    ("question", "Should I never use unwrap?", false),
    ("question", "why does it never stop?", false),
    ("question", "¿nunca hay que hacer push a main?", false),
    ("question", "do you want me to always commit?", false),
    ("question", "is it ok to push now", false),
    ("question", "¿puedo borrar la rama?", false),
    ("question", "what happens if I don't want the cache", false),
    ("question", "can you always see my screen?", false),
    ("question", "no entiendo por qué siempre responde en inglés", false),
    ("question", "does the build must run offline?", false),
    // quoted text, pasted logs, code
    ("pasted", "> never push to main", false),
    ("pasted", "// always run the tests", false),
    ("pasted", "ERROR: you must not call this twice", false),
    ("pasted", "[WARN] never use the legacy path", false),
    ("pasted", "2026-11-14 09:00:00 INFO make sure the cache is warm", false),
    ("pasted", "error[E0382]: borrow of moved value", false),
    ("pasted", "warning: unused variable, never read", false),
    ("pasted", "    assert!(must_not_fail());", false),
    ("pasted", "if never_called { return; }", false),
    ("pasted", "let always = true;", false),
    ("pasted", "fn never_panic() -> Result<(), Error> {", false),
    ("pasted", "thread 'main' panicked at src/lib.rs:12: must not be empty", false),
    ("pasted", "$ git push origin main --force", false),
    ("pasted", "Traceback (most recent call last):", false),
    ("pasted", "  File \"x.py\", line 3, in <module>", false),
    ("pasted", "<system-reminder>\nNever use tabs.\n</system-reminder>", false),
    ("pasted", "```\nalways answer in English\n```", false),
    ("pasted", "README says: \"you must always run make first\" but it does not", false),
    ("pasted", "\"Never go full stack\" is what the article says", false),
    ("pasted", "error: you must not borrow twice", false),
    // plain requests and chatter that are not rules
    ("plain", "Fix the parser", false),
    ("plain", "arregla el test que falla", false),
    ("plain", "thanks, looks good", false),
    ("plain", "vale, gracias", false),
    ("plain", "can you check the logs", false),
    ("plain", "I'm going to bed", false),
    ("plain", "mira el diff y dime qué opinas", false),
    ("plain", "what does this function do", false),
    ("plain", "The login page has a blue header", false),
    ("plain", "estoy probando la nueva versión", false),
];

#[test]
fn the_rule_battery_has_no_misses_and_no_false_hits() {
    let mut groups: Vec<&str> = LINES.iter().map(|l| l.0).collect();
    groups.dedup();
    let (mut miss_all, mut false_all) = (0, 0);
    let mut bad = Vec::new();
    println!("| group | lines | misses | false hits |\n|---|---|---|---|");
    for g in &groups {
        let rows: Vec<_> = LINES.iter().filter(|l| l.0 == *g).collect();
        let (mut miss, mut fh) = (0, 0);
        for (_, line, want) in &rows {
            let got = !rules_in(line).is_empty();
            if got != *want {
                if *want {
                    miss += 1
                } else {
                    fh += 1
                }
                bad.push(format!("{} {}: {line:?}", g, if *want { "MISS" } else { "FALSE-HIT" }));
            }
        }
        println!("| {g} | {} | {miss} | {fh} |", rows.len());
        miss_all += miss;
        false_all += fh;
    }
    println!("| total | {} | {miss_all} | {false_all} |", LINES.len());
    for b in &bad {
        println!("{b}");
    }
    assert!(bad.is_empty(), "{} wrong:\n{}", bad.len(), bad.join("\n"));
}
