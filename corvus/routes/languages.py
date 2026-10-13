"""Interface languages: what is installed, and the strings of one.

The files themselves are read by :mod:`corvus.languages`; the frontend that
applies them is ``src/js/i18n.js``.
"""
from __future__ import annotations

from urllib.parse import parse_qs, urlparse

from .. import languages
from ..http_routes import route


class LanguageRoutes:
    # Tests point these at a temporary folder; None means the real ones.
    language_user_dir: str | None = None
    language_bundled_dir: str | None = None

    def _language_roots(self) -> tuple[str, str]:
        return (self.language_user_dir or languages.user_languages_dir(),
                self.language_bundled_dir or languages.bundled_languages_dir())

    @route("GET", "/api/languages")
    def _api_languages(self) -> None:
        """The installed languages and the folder an operator adds one to.

        Always 200: with nothing on disk English is still listed. The folder
        is created (with its README) the first time it is asked about, so an
        operator who was told to drop a file there finds it.
        """
        if self.language_user_dir is None:
            languages.ensure_user_languages_dir()
        user_dir, bundled_dir = self._language_roots()
        self._send_json({
            "languages": languages.discover(user_dir, bundled_dir),
            "default": languages.DEFAULT_CODE,
            "user_dir": user_dir,
        })

    @route("GET", "/api/languages/strings")
    def _api_language_strings(self) -> None:
        """One language's strings: ``?code=de``.

        ``strings`` maps the English text to its translation. English answers
        with none, because the English text is the interface itself.
        """
        query = parse_qs(urlparse(getattr(self, "path", "") or "").query)
        code = (query.get("code") or [""])[0]
        if not languages.is_valid_code(code):
            self._send_json({"error": "code must be a language code such as de or pt-BR"}, 400)
            return
        user_dir, bundled_dir = self._language_roots()
        entry = languages.load(code, user_dir, bundled_dir)
        if entry is None:
            self._send_json({"error": f"language {code} is not installed"}, 404)
            return
        self._send_json({"code": entry["code"], "name": entry["name"],
                         "native_name": entry["native_name"], "source": entry["source"],
                         "strings": entry["strings"]})
