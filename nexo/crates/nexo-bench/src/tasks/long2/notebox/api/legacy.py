"""Version 1 routes, deprecated and kept for the old mobile app."""
from flask import Blueprint, abort, jsonify, request

bp = Blueprint("legacy", __name__)


@bp.route("/notes/<int:note_id>/share", methods=["POST"])
def share_v1(note_id):
    if request.cookies.get("PHPSESSID") is None:
        abort(401)
    if request.form.get("_csrf") is None:
        abort(403)
    return jsonify(shared=request.form.get("to"), level=request.form.get("level", "read"))
