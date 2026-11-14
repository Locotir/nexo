// nexo browser: the consent prompt. Only a real click on Allow, made by the person, lets a session in; Deny is the
// default button, and closing the window, waiting too long or anything unexpected is a no.
"use strict";

const ASK_SECONDS = 110;
const params = new URLSearchParams(location.search);
const sid = params.get("sid") || "";
document.getElementById("name").textContent = (params.get("name") || "nexo").slice(0, 40);

let done = false;
function answer(ok, event) {
  if (done) return;
  // A script cannot click for the person: only a trusted event allows.
  if (ok && !(event && event.isTrusted)) return;
  done = true;
  try {
    chrome.runtime.sendMessage({ t: "consent_answer", sid, ok }).catch(() => {}).finally(() => window.close());
  } catch (e) {
    window.close();
  }
}

document.getElementById("allow").addEventListener("click", (e) => answer(true, e));
document.getElementById("deny").addEventListener("click", (e) => answer(false, e));
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") answer(false, e);
});

let left = ASK_SECONDS;
const note = document.getElementById("left");
const tick = () => {
  note.textContent = "No answer in " + left + " s means no.";
  if (left-- <= 0) answer(false);
};
tick();
setInterval(tick, 1000);
