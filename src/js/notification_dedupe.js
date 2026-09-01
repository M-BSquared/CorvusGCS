"use strict";

window.Corvus = window.Corvus || {};

Corvus.notificationDedupe = (function () {
  function actionFromMessage(message) {
    const text = String(message || "").toLowerCase();
    if (/\bdisarm(?:ed|ing)?\b/.test(text)) return "disarm";
    if (/\barm(?:ed|ing)?\b/.test(text)) return "arm";
    if (/\btake[\s-]?off\b/.test(text)) return "takeoff";
    if (/\breturn\s+to\s+launch\b|\brtl\b/.test(text)) return "rtl";
    if (/\bland(?:ed|ing)?\b/.test(text)) return "land";
    if (/\bmode\b/.test(text)) return "mode";
    return "";
  }

  function isFailureMessage(message) {
    return /fail|reject|deni|unsupported|time(?:d)?\s*out|disconnect|unknown|did\s+not|\bno\s+(?:home|global|altitude|connection)|invalid|error|unavailable/i
      .test(String(message || ""));
  }

  function createTracker(options = {}) {
    const windowMs = options.windowMs || 3000;
    const now = options.now || (() => Date.now());
    const attempts = new Map();
    let nextId = 0;

    function begin(actions) {
      const attempt = {
        id: ++nextId,
        actions: new Set(Array.isArray(actions) ? actions : [actions]),
        startedAt: now(),
        completedAt: 0,
        status: "pending",
        remoteMatched: false,
        localNotificationId: null,
      };
      attempts.set(attempt.id, attempt);
      return attempt;
    }

    function succeeded(attempt) {
      if (!attempt) return;
      attempt.status = "succeeded";
      attempts.delete(attempt.id);
    }

    function failed(attempt) {
      if (!attempt) return;
      attempt.status = "failed";
      attempt.completedAt = now();
    }

    function remove(attempt) {
      if (attempt) attempts.delete(attempt.id);
    }

    function attachLocal(attempt, notificationId) {
      if (attempt) attempt.localNotificationId = notificationId;
    }

    function shouldAddLocal(attempt) {
      return !attempt || !attempt.remoteMatched;
    }

    function matchRemote(notification) {
      const action = actionFromMessage(notification?.msg);
      if (!action || !isFailureMessage(notification?.msg)) return null;
      const timestamp = now();
      const candidates = Array.from(attempts.values()).filter((candidate) => {
        if (candidate.remoteMatched || !candidate.actions.has(action)) return false;
        if (candidate.status === "pending") return true;
        return candidate.status === "failed" && timestamp - candidate.completedAt <= windowMs;
      });
      const localFirst = candidates
        .filter((candidate) => candidate.status === "failed" && candidate.localNotificationId)
        .sort((a, b) => a.startedAt - b.startedAt);
      const pending = candidates.filter((candidate) => candidate.status === "pending").reverse();
      const recentFailed = candidates.filter((candidate) => candidate.status === "failed").reverse();
      const attempt = localFirst[0] || pending[0] || recentFailed[0];
      if (!attempt) return null;
      attempt.remoteMatched = true;
      return attempt;
    }

    return { windowMs, begin, succeeded, failed, remove, attachLocal, shouldAddLocal, matchRemote };
  }

  return { actionFromMessage, isFailureMessage, createTracker };
})();
