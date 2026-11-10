# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
"""Command line: ``python -m vca_stt_server``. All settings come from ``VCA_STT_*``."""

from __future__ import annotations

import logging
import sys

from .config import Settings


def main() -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    settings = Settings.from_env()
    if settings.host not in ("127.0.0.1", "::1", "localhost"):
        logging.warning("listening on %s: put TLS in front and limit who can reach this port",
                        settings.host)
    import uvicorn

    from .app import create_app

    app = create_app(settings)
    uvicorn.run(
        app,
        host=settings.host,
        port=settings.port,
        server_header=False,
        access_log=False,      # the app writes its own, content-free request log
        proxy_headers=False,   # the Guard decides which proxies to trust
        log_level="warning",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
