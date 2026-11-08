from flask import Flask

import legacy
import shares
from middleware import require_csrf, require_json, require_session

app = Flask(__name__)
app.before_request(require_session)
app.before_request(require_csrf)
app.before_request(require_json)
app.register_blueprint(shares.bp, url_prefix="/api/v2")
app.register_blueprint(legacy.bp, url_prefix="/api/v1")

if __name__ == "__main__":
    app.run(port=5000)
