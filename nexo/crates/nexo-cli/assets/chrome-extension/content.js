// nexo browser: content script. Runs in every page and answers the service worker's small requests.
chrome.runtime.onMessage.addListener((msg, _sender, reply) => {
  if (msg && msg.type === "title") {
    reply({ title: document.title, url: location.href });
  }
});
