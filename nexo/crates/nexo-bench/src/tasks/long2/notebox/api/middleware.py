"""Checks every request must pass before a handler runs."""
from flask import abort, g, request

from settings import CLIENT_HEADER, CLIENT_VALUE, CSRF_HEADER, SESSION_COOKIE
from sessions import lookup


def require_session():
    if request.path.startswith("/api/v1/"):
        return  # the old routes do their own check, see legacy.py
    sid = request.cookies.get(SESSION_COOKIE)
    session = lookup(sid) if sid else None
    if session is None:
        abort(401)
    g.session = session


def require_csrf():
    if request.method in ("GET", "HEAD") or request.path.startswith("/api/v1/"):
        return
    if request.headers.get(CSRF_HEADER) != g.session["csrf"]:
        abort(403)
    if request.headers.get(CLIENT_HEADER) != CLIENT_VALUE:
        abort(400)


def require_json():
    if request.method == "POST" and request.path.startswith("/api/v2/"):
        if request.mimetype != "application/json":
            abort(415)
