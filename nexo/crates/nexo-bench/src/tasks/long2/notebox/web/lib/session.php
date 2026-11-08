<?php
require_once __DIR__ . '/../config.php';

// One session per browser; the name is set before it starts so the cookie carries it.
function start_notebox_session(): void {
    session_name(SESSION_NAME);
    session_start();
    if (empty($_SESSION['csrf'])) {
        $_SESSION['csrf'] = bin2hex(random_bytes(16));
    }
}

function csrf_token(): string {
    return $_SESSION['csrf'];
}

function current_user(): string {
    return $_SESSION['user'] ?? 'guest';
}
