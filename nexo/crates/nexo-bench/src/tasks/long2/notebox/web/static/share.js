// Opens the share dialog for a note and sends the form to the API.
document.querySelectorAll('.share-open').forEach((b) => {
  b.addEventListener('click', () => {
    document.getElementById('share-form').dataset.note = b.dataset.note;
  });
});

async function shareNote(form) {
  const noteId = form.dataset.note;
  const data = new FormData(form);
  const notify = data.get('notify') ? 1 : 0;
  const url = `${NB.apiBase}/notes/${noteId}/shares?notify=${notify}`;
  const headers = {
    'Content-Type': 'application/json',
    'X-NB-Client': 'web',
  };
  headers[NB.csrfHeader] = NB.csrf;
  const body = {
    recipient_email: data.get('recipient'),
    permission: data.get('level'),
    expires_days: parseInt(data.get('days'), 10),
  };
  const res = await fetch(url, {
    method: 'POST',
    credentials: 'same-origin',
    headers,
    body: JSON.stringify(body),
  });
  return res.ok;
}

document.getElementById('share-form').addEventListener('submit', (e) => {
  e.preventDefault();
  shareNote(e.target);
});
