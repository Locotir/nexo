"""A tiny in-memory session table."""

_SESSIONS = {}


def lookup(sid):
    return _SESSIONS.get(sid)


def create(sid, user, csrf):
    _SESSIONS[sid] = {"user": user, "csrf": csrf}
