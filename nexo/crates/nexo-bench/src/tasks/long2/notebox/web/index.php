<?php
require_once __DIR__ . '/lib/session.php';
start_notebox_session();
$notes = [
    ['id' => 42, 'title' => 'Weekly plan'],
    ['id' => 43, 'title' => 'Reading list'],
];
?>
<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <title>Notebox</title>
  <script>
    window.NB = {
      apiBase: "<?= API_BASE ?>",
      csrfHeader: "<?= CSRF_HEADER ?>",
      csrf: "<?= csrf_token() ?>"
    };
  </script>
  <script src="/static/share.js" defer></script>
</head>
<body>
  <h1>Notes of <?= htmlspecialchars(current_user()) ?></h1>
  <ul>
  <?php foreach ($notes as $n): ?>
    <li data-note-id="<?= $n['id'] ?>"><?= htmlspecialchars($n['title']) ?>
      <button class="share-open" data-note="<?= $n['id'] ?>">Share</button></li>
  <?php endforeach; ?>
  </ul>

  <form id="share-form" data-note="">
    <input type="email" name="recipient" placeholder="Who gets it?" required>
    <select name="level">
      <option value="viewer">Can read</option>
      <option value="editor">Can edit</option>
    </select>
    <input type="number" name="days" value="7" min="1">
    <label><input type="checkbox" name="notify" checked> Tell them by email</label>
    <button type="submit">Share note</button>
  </form>
</body>
</html>
