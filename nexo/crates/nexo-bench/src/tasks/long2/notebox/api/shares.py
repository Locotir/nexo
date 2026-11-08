"""Sharing a note with another person (API version 2)."""
from flask import Blueprint, abort, g, jsonify, request

from settings import MAX_SHARE_DAYS, PERMISSIONS

bp = Blueprint("shares", __name__)

SHARES = []


@bp.route("/notes/<int:note_id>/shares", methods=["POST"])
def create_share(note_id):
    body = request.get_json(silent=True) or {}
    email = body.get("recipient_email", "")
    permission = body.get("permission")
    days = body.get("expires_days")
    if "@" not in email or permission not in PERMISSIONS:
        abort(422)
    if not isinstance(days, int) or not 1 <= days <= MAX_SHARE_DAYS:
        abort(422)
    notify = request.args.get("notify", "0") == "1"
    SHARES.append((note_id, email, permission, days, notify, g.session["user"]))
    return jsonify(ok=True), 201


def share_via_link(note_id):
    """Never registered: link sharing was dropped before release."""
    token = request.headers.get("X-Share-Token")
    return jsonify(link=f"/s/{note_id}/{token}")
