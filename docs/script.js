/*
  Corvus GCS project website, and the frame of the manual in docs/guide.

  Everything here is an enhancement, nothing the page depends on to be
  readable: the theme switch, the mobile menu, the section the menu marks as
  current, the workflow that changes picture as you read, the screenshot
  viewer and its lightbox, and one optional call to the GitHub releases API.
  If that call fails the download section keeps the static text it shipped
  with, so the page is complete with no network beyond itself.

  Every part checks for its own elements first, because the manual's pages
  load this file too and have none of the landing page's sections.
*/
(function () {
  'use strict';

  var REPO = 'M-BSquared/CorvusGCS';
  var root = document.documentElement;
  var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');

  function $(id) { return document.getElementById(id); }
  function all(selector, scope) {
    return Array.prototype.slice.call((scope || document).querySelectorAll(selector));
  }

  /* ---------------------------------------------------------------
     Theme. The stored choice wins; otherwise dark, as the page ships.
     The <head> applies it before first paint; this only wires the
     button and keeps the two in step.
     --------------------------------------------------------------- */
  var themeToggle = $('theme-toggle');
  var heroImage = document.getElementById('hero-image');

  /* The hero screenshot wears the page's theme: the application in one of
     its dark themes on the dark page, in its light default on the light one. */
  function applyHeroImage(theme) {
    if (!heroImage) return;
    var base = heroImage.getAttribute('data-' + theme);
    if (!base || heroImage.getAttribute('data-shown') === theme) return;
    heroImage.setAttribute('data-shown', theme);
    heroImage.srcset = base + '-800.jpg 800w, ' + base + '.jpg 1600w';
    heroImage.src = base + '-800.jpg';
  }

  function applyTheme(theme) {
    root.setAttribute('data-theme', theme);
    applyHeroImage(theme);
    if (themeToggle) {
      var light = theme === 'light';
      themeToggle.setAttribute('aria-pressed', String(light));
      themeToggle.setAttribute(
        'aria-label',
        light ? 'Switch to the dark theme' : 'Switch to the light theme'
      );
    }
  }

  applyTheme(root.getAttribute('data-theme') === 'light' ? 'light' : 'dark');

  if (themeToggle) {
    themeToggle.addEventListener('click', function () {
      var next = root.getAttribute('data-theme') === 'light' ? 'dark' : 'light';
      applyTheme(next);
      try {
        localStorage.setItem('corvus-theme', next);
      } catch (err) {
        /* Private mode or blocked storage: the choice just does not persist. */
      }
    });
  }

  /* ---------------------------------------------------------------
     Scroll: one frame callback for what follows the page's position.
     --------------------------------------------------------------- */
  var header = $('site-header');
  var scrollers = [];
  var scrollTick = false;

  function onScroll() {
    if (scrollTick) return;
    scrollTick = true;
    window.requestAnimationFrame(function () {
      scrollTick = false;
      for (var i = 0; i < scrollers.length; i++) scrollers[i]();
    });
  }

  /* The hairline under the header shows how far down the page the reader is. */
  if (header) {
    scrollers.push(function () {
      var max = document.documentElement.scrollHeight - window.innerHeight;
      var p = max > 0 ? Math.min(1, Math.max(0, window.scrollY / max)) : 0;
      header.style.setProperty('--progress', p.toFixed(4));
    });
  }

  window.addEventListener('scroll', onScroll, { passive: true });
  window.addEventListener('resize', onScroll);

  /* ---------------------------------------------------------------
     Mobile navigation.
     --------------------------------------------------------------- */
  var navToggle = $('nav-toggle');
  var nav = $('site-nav');

  function setNav(open) {
    if (!nav || !navToggle) return;
    nav.classList.toggle('is-open', open);
    navToggle.setAttribute('aria-expanded', String(open));
    navToggle.setAttribute('aria-label', open ? 'Close the menu' : 'Open the menu');
  }

  if (navToggle && nav) {
    navToggle.addEventListener('click', function () {
      setNav(navToggle.getAttribute('aria-expanded') !== 'true');
    });

    nav.addEventListener('click', function (event) {
      if (event.target.closest('a')) setNav(false);
    });

    document.addEventListener('keydown', function (event) {
      if (event.key === 'Escape' && navToggle.getAttribute('aria-expanded') === 'true') {
        setNav(false);
        navToggle.focus();
      }
    });

    window.addEventListener('resize', function () {
      if (window.innerWidth > 1024) setNav(false);
    });
  }

  /* ---------------------------------------------------------------
     The menu marks the section being read.
     --------------------------------------------------------------- */
  var spyLinks = nav ? all('.nav-list a[href^="#"]', nav) : [];

  if (spyLinks.length && 'IntersectionObserver' in window) {
    var spyTargets = [];
    spyLinks.forEach(function (link) {
      var target = $(link.getAttribute('href').slice(1));
      if (target) spyTargets.push({ link: link, target: target });
    });
    var spy = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        spyTargets.forEach(function (item) {
          if (item.target === entry.target) item.link.classList.toggle('is-active', entry.isIntersecting);
        });
      });
    }, { rootMargin: '-45% 0px -50% 0px' });
    spyTargets.forEach(function (item) { spy.observe(item.target); });
  }

  /* ---------------------------------------------------------------
     Reveal on scroll: content rises a little as it comes into view,
     siblings one after another. Without IntersectionObserver, or with
     reduced motion asked for, everything is simply shown.
     --------------------------------------------------------------- */
  var revealables = all('.reveal');

  revealables.forEach(function (el) {
    var siblings = el.parentElement ? all(':scope > .reveal', el.parentElement) : [];
    if (siblings.length > 1) {
      el.style.setProperty('--reveal-delay', ((siblings.indexOf(el) % 3) * 0.07).toFixed(2) + 's');
    }
  });

  function showAll() {
    revealables.forEach(function (el) { el.classList.add('is-visible'); });
  }

  if (reduceMotion.matches || !('IntersectionObserver' in window)) {
    showAll();
  } else {
    var revealObserver = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible');
          revealObserver.unobserve(entry.target);
        }
      });
    }, { rootMargin: '0px 0px -6% 0px', threshold: 0.05 });
    revealables.forEach(function (el) { revealObserver.observe(el); });
  }

  reduceMotion.addEventListener('change', function (event) {
    if (event.matches) showAll();
  });
  window.addEventListener('beforeprint', showAll);

  /* ---------------------------------------------------------------
     The hero: an aircraft flies the route drawn on the map sheet, the
     flown track trails it, and each numbered waypoint fills in as it
     is passed. It only runs while the hero is on screen.
     --------------------------------------------------------------- */
  var hero = $('hero');
  var routePath = $('route-path');
  var routeTrack = $('route-track');
  var routeCraft = $('route-craft');

  if (hero && routePath && routeTrack && routeCraft && routePath.getTotalLength) {
    var LAP_MS = 32000;
    var total = routePath.getTotalLength();
    var waypoints = all('.route-wp', hero);

    /* Where along the route each marker sits, found by sampling. */
    var wpAt = waypoints.map(function (wp) {
      var m = /translate\(([\d.]+)[ ,]+([\d.]+)\)/.exec(wp.getAttribute('transform') || '');
      if (!m) return total;
      var x = parseFloat(m[1]), y = parseFloat(m[2]);
      var best = 0, bestD = Infinity;
      for (var s = 0; s <= 600; s++) {
        var p = routePath.getPointAtLength(total * s / 600);
        var d = (p.x - x) * (p.x - x) + (p.y - y) * (p.y - y);
        if (d < bestD) { bestD = d; best = total * s / 600; }
      }
      return best;
    });

    var running = false;
    var visible = true;
    var startAt = performance.now() - LAP_MS * 0.18;

    function frame(now) {
      var p = ((now - startAt) % LAP_MS) / LAP_MS;
      var len = p * total;
      var pt = routePath.getPointAtLength(len);
      var ahead = routePath.getPointAtLength((len + 6) % total);
      var angle = Math.atan2(ahead.y - pt.y, ahead.x - pt.x) * 180 / Math.PI;

      routeCraft.setAttribute('transform',
        'translate(' + pt.x.toFixed(1) + ' ' + pt.y.toFixed(1) + ') rotate(' + angle.toFixed(1) + ')');
      routeTrack.style.strokeDashoffset = (0.16 - p).toFixed(4);
      waypoints.forEach(function (wp, i) { wp.classList.toggle('is-passed', wpAt[i] <= len); });

      if (running) window.requestAnimationFrame(frame);
    }

    function setRunning(on) {
      on = on && visible && !document.hidden && !reduceMotion.matches;
      if (on === running) return;
      running = on;
      if (on) window.requestAnimationFrame(frame);
    }

    /* A still frame part of the way round, for reduced motion and for the
       moment before the first animation frame. */
    frame(startAt + LAP_MS * 0.42);

    if ('IntersectionObserver' in window) {
      new IntersectionObserver(function (entries) {
        visible = entries[0].isIntersecting;
        setRunning(true);
      }).observe(hero);
    }
    document.addEventListener('visibilitychange', function () { setRunning(true); });
    reduceMotion.addEventListener('change', function () { setRunning(true); });
    setRunning(true);
  }


  /* ---------------------------------------------------------------
     Pictures lean a little toward the pointer. Each one has a resting
     angle (the hero and the story picture) or none, follows the pointer
     over its zone within a few degrees, and settles back when the
     pointer leaves. The hero flattens as the page scrolls. Only with a
     mouse, on a wide screen and without reduced motion.
     --------------------------------------------------------------- */
  var finePointer = window.matchMedia('(hover: hover) and (pointer: fine)');

  var tilts = [
    { el: '.hero-shot', zone: '#hero', rest: [4, -7], range: [6, 8], flatten: true },
    { el: '.flow-frame', zone: '.flow-frame', rest: [0, 0], range: [5, 7] },
    { el: '.viewer-frame', zone: '.viewer-stage', rest: [0, 0], range: [4, 6] },
    { el: '.story-shot .mac-window', zone: '.story-shot', rest: [3, -6], range: [5, 7] }
  ].map(function (t) {
    t.node = document.querySelector(t.el);
    t.area = document.querySelector(t.zone);
    t.nx = 0; t.ny = 0; t.inside = false;
    return t;
  }).filter(function (t) { return t.node && t.area; });

  function tiltOn() {
    return finePointer.matches && !reduceMotion.matches && window.innerWidth > 1024;
  }

  function applyTilt(t) {
    if (!tiltOn()) { t.node.style.transform = ''; return; }
    var flat = t.flatten ? 1 - Math.min(1, Math.max(0, window.scrollY / 700)) : 1;
    var rx = (t.rest[0] - t.ny * t.range[0]) * flat;
    var ry = (t.rest[1] + t.nx * t.range[1]) * flat;
    t.node.style.transform = 'rotateX(' + rx.toFixed(2) + 'deg) rotateY(' + ry.toFixed(2) + 'deg)';
  }

  tilts.forEach(function (t) {
    t.node.classList.add('tilt');
    if (t.node.parentElement) t.node.parentElement.style.perspective = '1800px';
    var pending = false;

    t.area.addEventListener('pointermove', function (event) {
      if (event.pointerType !== 'mouse' || !tiltOn()) return;
      var r = t.node.getBoundingClientRect();
      t.nx = Math.min(1, Math.max(-1, (event.clientX - (r.left + r.width / 2)) / (r.width / 2)));
      t.ny = Math.min(1, Math.max(-1, (event.clientY - (r.top + r.height / 2)) / (r.height / 2)));
      if (pending) return;
      pending = true;
      window.requestAnimationFrame(function () { pending = false; applyTilt(t); });
    });
    t.area.addEventListener('pointerleave', function () {
      t.nx = 0; t.ny = 0;
      applyTilt(t);
    });
    if (t.flatten) scrollers.push(function () { applyTilt(t); });
    applyTilt(t);
  });

  window.addEventListener('resize', function () { tilts.forEach(applyTilt); });
  reduceMotion.addEventListener('change', function () { tilts.forEach(applyTilt); });

  /* ---------------------------------------------------------------
     The headline cycles through what Corvus is for, with a row of
     short bars under it that fill while each statement is shown.
     It holds still off screen, in a background tab and with reduced
     motion, where the first statement simply stays.
     --------------------------------------------------------------- */
  var rotator = $('hero-rotator');

  if (rotator) {
    var lines = all('.rotator-line', rotator);
    var HOLD_MS = 4200;
    var at = 0;
    var timer = null;
    var onScreen = true;
    var bar = document.createElement('div');
    bar.className = 'rotator-progress';
    bar.setAttribute('aria-hidden', 'true');
    bar.style.setProperty('--rotate-ms', HOLD_MS + 'ms');
    var marks = lines.map(function () { return bar.appendChild(document.createElement('span')); });
    rotator.parentElement.insertAdjacentElement('afterend', bar);

    var markCurrent = function () {
      marks.forEach(function (m, i) {
        m.classList.toggle('is-done', i < at);
        m.classList.remove('is-current');
      });
      void marks[at].offsetWidth;
      marks[at].classList.add('is-current');
    };

    var nextLine = function () {
      var old = lines[at];
      at = (at + 1) % lines.length;
      var line = lines[at];
      old.classList.remove('is-current');
      old.classList.add('is-leaving');
      window.setTimeout(function () { old.classList.remove('is-leaving'); }, 750);
      line.classList.add('is-entering', 'is-current');
      void line.offsetWidth;
      line.classList.remove('is-entering');
      markCurrent();
    };

    var schedule = function () {
      var run = onScreen && !document.hidden && !reduceMotion.matches && lines.length > 1;
      bar.hidden = reduceMotion.matches;
      if (run === (timer !== null)) return;
      window.clearInterval(timer);
      timer = null;
      if (run) {
        markCurrent();
        timer = window.setInterval(nextLine, HOLD_MS);
      }
    };

    if ('IntersectionObserver' in window) {
      new IntersectionObserver(function (entries) {
        onScreen = entries[0].isIntersecting;
        schedule();
      }).observe(rotator);
    }
    document.addEventListener('visibilitychange', schedule);
    reduceMotion.addEventListener('change', schedule);
    schedule();
  }

  /* ---------------------------------------------------------------
     Workflow: the step in the middle of the screen is the current
     one and its picture shows.
     --------------------------------------------------------------- */
  var flow = $('flow');

  if (flow) {
    var steps = all('.flow-step', flow);
    var flowShots = all('.flow-shot', flow);
    var flowTitle = $('flow-title');
    var flowList = flow.querySelector('.flow-steps');
    var currentStep = 0;

    var setStep = function (index) {
      if (index === currentStep) return;
      currentStep = index;
      steps.forEach(function (step, i) {
        step.classList.toggle('is-active', i === index);
        step.classList.toggle('is-passed', i < index);
      });
      flowShots.forEach(function (shot, i) {
        shot.classList.toggle('is-active', i === index);
        if (i === index && flowTitle) flowTitle.textContent = shot.getAttribute('data-title') || '';
      });
    };

    /* The pictures are lazy, so the next ones are asked for in advance:
       a crossfade to a picture still on its way is a crossfade to nothing. */
    if ('IntersectionObserver' in window) {
      new IntersectionObserver(function (entries, obs) {
        if (!entries[0].isIntersecting) return;
        flowShots.forEach(function (shot) { shot.loading = 'eager'; });
        obs.disconnect();
      }, { rootMargin: '600px 0px' }).observe(flow);
    }

    scrollers.push(function () {
      var mid = window.innerHeight * 0.5;
      var rect = flowList.getBoundingClientRect();
      var done = rect.height > 0 ? (mid - rect.top) / rect.height : 0;
      flowList.style.setProperty('--flow-progress', Math.min(1, Math.max(0, done)).toFixed(4));
      var index = 0;
      steps.forEach(function (step, i) {
        if (step.getBoundingClientRect().top < mid) index = i;
      });
      setStep(index);
    });
    onScroll();
  }

  /* ---------------------------------------------------------------
     Screenshot viewer. Tabs pick a group; a row in the list puts its
     picture on the stage; the stage opens the lightbox, which steps
     through the same group and keeps the list in step with it.
     --------------------------------------------------------------- */
  var tablist = $('gallery-tabs');
  var tabs = tablist ? all('[role="tab"]', tablist) : [];
  var groups = tabs.map(function (tab) { return $(tab.getAttribute('aria-controls')); });
  var stageButton = $('viewer-open');
  var stageImage = $('viewer-image');
  var stageCaption = $('viewer-caption');
  var stageCount = $('viewer-count');
  var activeGroup = 0;
  var picked = groups.map(function () { return 0; });

  function groupShots(index) {
    return groups[index] ? all('.shot-btn', groups[index]) : [];
  }

  function copyCaption(button, into) {
    var caption = button.closest('figure').querySelector('figcaption');
    into.textContent = '';
    if (!caption) return;
    Array.prototype.forEach.call(caption.childNodes, function (node) {
      into.appendChild(node.cloneNode(true));
    });
  }

  /* Scrolls only the list it is in, never the page. */
  function keepInView(figure) {
    var group = groups[activeGroup];
    var gallery = figure.parentElement;
    if (!group || !gallery) return;
    if (gallery.scrollWidth > gallery.clientWidth + 4) {
      var left = figure.offsetLeft - gallery.offsetLeft;
      if (left < gallery.scrollLeft || left + figure.offsetWidth > gallery.scrollLeft + gallery.clientWidth) {
        gallery.scrollTo({ left: left - 12, behavior: reduceMotion.matches ? 'auto' : 'smooth' });
      }
    } else if (group.scrollHeight > group.clientHeight + 4) {
      var top = figure.offsetTop - group.offsetTop;
      if (top < group.scrollTop || top + figure.offsetHeight > group.scrollTop + group.clientHeight - 36) {
        group.scrollTo({ top: top - 40, behavior: reduceMotion.matches ? 'auto' : 'smooth' });
      }
    }
  }

  function pickShot(index, follow) {
    var shots = groupShots(activeGroup);
    if (!shots.length || !stageImage) return;
    index = (index + shots.length) % shots.length;
    picked[activeGroup] = index;

    shots.forEach(function (btn, i) {
      var current = i === index;
      btn.closest('figure').classList.toggle('is-current', current);
      if (current) btn.setAttribute('aria-current', 'true');
      else btn.removeAttribute('aria-current');
    });

    var btn = shots[index];
    var thumb = btn.querySelector('img');
    var full = btn.getAttribute('data-full');
    if (stageImage.getAttribute('data-full') !== full) {
      stageButton.classList.add('is-swapping');
      var next = new Image();
      next.onload = next.onerror = function () {
        if (stageImage.getAttribute('data-full') !== full) return;
        stageButton.classList.remove('is-swapping');
      };
      stageImage.setAttribute('data-full', full);
      stageImage.sizes = '(max-width: 900px) calc(100vw - 40px), 900px';
      stageImage.srcset = thumb.getAttribute('srcset');
      stageImage.src = thumb.getAttribute('src');
      stageImage.alt = thumb.alt;
      next.sizes = stageImage.sizes;
      next.srcset = stageImage.srcset;
      next.src = stageImage.src;
    }
    copyCaption(btn, stageCaption);
    var heading = btn.closest('figure').querySelector('figcaption strong');
    var stageTitle = $('viewer-title');
    if (stageTitle && heading) stageTitle.textContent = heading.textContent.replace(/\.$/, '');
    stageCount.textContent = groups[activeGroup].getAttribute('data-group') + ' · ' +
      (index + 1) + ' / ' + shots.length;
    if (follow) keepInView(btn.closest('figure'));
    markDots(index);
    restartDwell();
  }

  function moveIndicator() {
    var tab = tabs[activeGroup];
    if (!tab || !tablist) return;
    tablist.style.setProperty('--tab-x', tab.offsetLeft + 'px');
    tablist.style.setProperty('--tab-w', tab.offsetWidth + 'px');
    tablist.style.setProperty('--tab-y', tab.offsetTop + 'px');
    tablist.style.setProperty('--tab-h', tab.offsetHeight + 'px');
  }

  function selectTab(index, focus) {
    activeGroup = index;
    tabs.forEach(function (tab, i) {
      var on = i === index;
      tab.setAttribute('aria-selected', String(on));
      tab.tabIndex = on ? 0 : -1;
      if (groups[i]) groups[i].hidden = !on;
    });
    if (focus) tabs[index].focus();
    moveIndicator();
    buildDots(index);
    pickShot(picked[index], false);
    if (groups[index]) groups[index].scrollTop = 0;
  }

  if (tabs.length && stageImage) {
    tabs.forEach(function (tab, i) {
      tab.addEventListener('click', function () { selectTab(i, false); });
    });
    tablist.addEventListener('keydown', function (event) {
      var next = null;
      if (event.key === 'ArrowRight') next = (activeGroup + 1) % tabs.length;
      if (event.key === 'ArrowLeft') next = (activeGroup - 1 + tabs.length) % tabs.length;
      if (event.key === 'Home') next = 0;
      if (event.key === 'End') next = tabs.length - 1;
      if (next === null) return;
      event.preventDefault();
      selectTab(next, true);
    });

    groups.forEach(function (group, g) {
      if (!group) return;
      all('.shot-btn', group).forEach(function (btn, i) {
        btn.addEventListener('click', function () {
          if (g !== activeGroup) selectTab(g, false);
          pickShot(i, false);
        });
      });
    });

    stageButton.addEventListener('click', function () {
      openLightbox(picked[activeGroup], stageButton);
    });

    window.addEventListener('resize', moveIndicator);
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(moveIndicator);
    selectTab(0, false);
  }

  /* ---------------------------------------------------------------
     The viewer plays through on its own: each picture stays for a few
     seconds, then the next one follows, and after the last of a group
     the next group starts. It plays only while the viewer is on screen,
     not under keyboard focus, not in a background tab and not with the
     lightbox open. Hovering does not stop it. Pause stops it for good, and with
     reduced motion asked for it never starts by itself.
     --------------------------------------------------------------- */
  var viewer = $('viewer');
  var dotsBox = $('viewer-dots');
  var playBtn = $('viewer-play');
  var prevBtn = $('viewer-prev');
  var nextBtn = $('viewer-next');
  var DWELL_MS = 3400;
  var dwell = 0;
  var lastFrame = 0;
  var frameId = 0;
  var userPaused = reduceMotion.matches;
  var onScreen = false;

  function currentDot() {
    return dotsBox ? dotsBox.querySelector('.viewer-dot.is-current') : null;
  }

  function buildDots(group) {
    if (!dotsBox) return;
    dotsBox.textContent = '';
    groupShots(group).forEach(function (btn, i) {
      var dot = document.createElement('button');
      dot.type = 'button';
      dot.className = 'viewer-dot';
      dot.setAttribute('aria-label', 'Screenshot ' + (i + 1));
      dot.addEventListener('click', function () { pickShot(i, true); });
      dotsBox.appendChild(dot);
    });
  }

  function markDots(index) {
    if (!dotsBox) return;
    all('.viewer-dot', dotsBox).forEach(function (dot, i) {
      dot.classList.toggle('is-current', i === index);
      dot.classList.toggle('is-seen', i < index);
      if (i === index) dot.setAttribute('aria-current', 'true');
      else dot.removeAttribute('aria-current');
    });
  }

  function setProgress(p) {
    var dot = currentDot();
    if (dot) dot.style.setProperty('--dot-progress', p.toFixed(3));
    var row = groups[activeGroup] ? groups[activeGroup].querySelector('.shot.is-current') : null;
    if (row) row.style.setProperty('--row-progress', p.toFixed(3));
  }

  /* Next or previous picture, crossing into the next group at the end. */
  function step(direction) {
    var count = groupShots(activeGroup).length;
    var to = picked[activeGroup] + direction;
    if (to >= count) {
      selectTab((activeGroup + 1) % tabs.length, false);
      pickShot(0, true);
    } else if (to < 0) {
      var before = (activeGroup - 1 + tabs.length) % tabs.length;
      selectTab(before, false);
      pickShot(groupShots(before).length - 1, true);
    } else {
      pickShot(to, true);
    }
  }

  function playing() {
    return !userPaused && onScreen && !document.hidden &&
      (!lightbox || lightbox.hidden) && !(viewer && viewer.contains(document.activeElement) &&
      document.activeElement.matches(':focus-visible'));
  }

  function tick(now) {
    frameId = 0;
    if (!playing()) { lastFrame = 0; return; }
    if (lastFrame) dwell += now - lastFrame;
    lastFrame = now;
    if (dwell >= DWELL_MS) {
      step(1);
      return syncPlayback();
    }
    setProgress(dwell / DWELL_MS);
    frameId = window.requestAnimationFrame(tick);
  }

  function syncPlayback() {
    if (playing()) {
      if (!frameId) { lastFrame = 0; frameId = window.requestAnimationFrame(tick); }
    } else if (frameId) {
      window.cancelAnimationFrame(frameId);
      frameId = 0;
      lastFrame = 0;
    }
    if (!playing()) setProgress(dwell ? dwell / DWELL_MS : 1);
  }

  function restartDwell() {
    dwell = 0;
    lastFrame = 0;
    setProgress(playing() ? 0 : 1);
    syncPlayback();
  }

  function setPaused(paused) {
    userPaused = paused;
    if (playBtn) {
      playBtn.setAttribute('aria-pressed', String(paused));
      var label = playBtn.querySelector('.viewer-play-label');
      if (label) label.textContent = paused ? 'Play' : 'Pause';
    }
    if (paused) setProgress(1);
    else dwell = 0;
    syncPlayback();
  }

  if (viewer && tabs.length && stageImage) {
    if (prevBtn) prevBtn.addEventListener('click', function () { step(-1); });
    if (nextBtn) nextBtn.addEventListener('click', function () { step(1); });
    if (playBtn) playBtn.addEventListener('click', function () { setPaused(!userPaused); });

    viewer.addEventListener('focusin', syncPlayback);
    viewer.addEventListener('focusout', function () { window.setTimeout(syncPlayback, 0); });
    viewer.addEventListener('keydown', function (event) {
      if (event.key === 'ArrowRight' && event.target.closest('.viewer-stage')) { event.preventDefault(); step(1); }
      if (event.key === 'ArrowLeft' && event.target.closest('.viewer-stage')) { event.preventDefault(); step(-1); }
    });
    document.addEventListener('visibilitychange', syncPlayback);

    if ('IntersectionObserver' in window) {
      new IntersectionObserver(function (entries) {
        onScreen = entries[0].isIntersecting;
        syncPlayback();
      }, { threshold: 0.35 }).observe(viewer);
    }
    buildDots(activeGroup);
    markDots(picked[activeGroup]);
    setPaused(userPaused);
    restartDwell();
  }

  /* ---------------------------------------------------------------
     Lightbox. Opens from the stage, closes on the button, Escape or
     the backdrop, and steps with the arrow keys. Focus goes in on
     open and back to what opened it.
     --------------------------------------------------------------- */
  var lightbox = $('lightbox');
  var lbImage = $('lightbox-image');
  var lbCaption = $('lightbox-caption');
  var lbCount = $('lightbox-count');
  var lbPrev = $('lightbox-prev');
  var lbNext = $('lightbox-next');
  var lbClose = $('lightbox-close');
  var lbIndex = 0;
  var opener = null;

  function preload(index) {
    var shots = groupShots(activeGroup);
    var btn = shots[(index + shots.length) % shots.length];
    if (btn) new Image().src = btn.getAttribute('data-full');
  }

  function show(index) {
    var shots = groupShots(activeGroup);
    if (!shots.length) return;
    lbIndex = (index + shots.length) % shots.length;
    var btn = shots[lbIndex];
    var img = btn.querySelector('img');

    lbImage.src = btn.getAttribute('data-full');
    lbImage.alt = img ? img.alt : '';
    /* The figure's own caption, title included, so each one is written once. */
    copyCaption(btn, lbCaption);
    lbCount.textContent = groups[activeGroup].getAttribute('data-group') + ' · ' +
      (lbIndex + 1) + ' / ' + shots.length;
    /* The list behind follows, so closing lands on the picture last seen. */
    pickShot(lbIndex, true);
    /* Stepping through should not wait on the network each time. */
    preload(lbIndex + 1);
    preload(lbIndex - 1);
  }

  function openLightbox(index, source) {
    if (!lightbox) return;
    opener = source || null;
    show(index);
    lightbox.hidden = false;
    document.body.style.overflow = 'hidden';
    lbClose.focus();
  }

  function closeLightbox() {
    if (!lightbox || lightbox.hidden) return;
    lightbox.hidden = true;
    document.body.style.overflow = '';
    if (opener) opener.focus();
    opener = null;
  }

  if (lightbox) {
    lbPrev.addEventListener('click', function () { show(lbIndex - 1); });
    lbNext.addEventListener('click', function () { show(lbIndex + 1); });

    lightbox.addEventListener('click', function (event) {
      if (event.target.hasAttribute('data-close') || event.target.closest('[data-close]')) {
        closeLightbox();
      }
    });

    document.addEventListener('keydown', function (event) {
      if (lightbox.hidden) return;

      if (event.key === 'Escape') {
        event.preventDefault();
        closeLightbox();
        return;
      }
      if (event.key === 'ArrowLeft') { event.preventDefault(); show(lbIndex - 1); return; }
      if (event.key === 'ArrowRight') { event.preventDefault(); show(lbIndex + 1); return; }

      /* Keep Tab inside the dialog while it is the only thing on screen. */
      if (event.key === 'Tab') {
        var focusable = [lbPrev, lbNext, lbClose];
        var current = focusable.indexOf(document.activeElement);
        var next = event.shiftKey ? current - 1 : current + 1;
        if (current === -1 || next < 0 || next >= focusable.length) {
          event.preventDefault();
          focusable[event.shiftKey ? focusable.length - 1 : 0].focus();
        }
      }
    });
  }

  /* ---------------------------------------------------------------
     The download entry for the visitor's own system is marked. A
     tablet that reports itself as a Mac is left alone.
     --------------------------------------------------------------- */
  var platformCards = all('.platforms [data-os]');

  if (platformCards.length) {
    var hint = ((navigator.userAgentData && navigator.userAgentData.platform) ||
      navigator.platform || navigator.userAgent || '').toLowerCase();
    var ua = (navigator.userAgent || '').toLowerCase();
    var os = null;
    if (/android|iphone|ipad/.test(ua) || (/mac/.test(hint) && navigator.maxTouchPoints > 1)) os = null;
    else if (/mac/.test(hint)) os = 'mac';
    else if (/win/.test(hint)) os = 'windows';
    else if (/linux/.test(hint)) os = 'linux';

    platformCards.forEach(function (card) {
      if (card.getAttribute('data-os') !== os) return;
      card.classList.add('is-yours');
      var badge = document.createElement('span');
      badge.className = 'platform-yours';
      badge.innerHTML = 'This computer';
      card.appendChild(badge);
    });
  }

  /* ---------------------------------------------------------------
     Latest release. Shown only when GitHub answers with a real tag;
     otherwise the line keeps the text it was served with.
     --------------------------------------------------------------- */
  var releaseLine = $('release-line');
  var heroRelease = $('hero-release');
  var heroTag = $('hero-release-tag');

  if ((releaseLine || heroRelease) && 'fetch' in window) {
    fetch('https://api.github.com/repos/' + REPO + '/releases/latest', {
      headers: { Accept: 'application/vnd.github+json' }
    })
      .then(function (response) {
        if (!response.ok) throw new Error('releases: ' + response.status);
        return response.json();
      })
      .then(function (release) {
        var tag = typeof release.tag_name === 'string' ? release.tag_name : '';
        if (!tag) return;

        // The hero names the newest release and links to its notes.
        if (heroRelease && heroTag) {
          heroTag.textContent = tag;
          var notes = typeof release.html_url === 'string' ? release.html_url : '';
          if (/^https:\/\/github\.com\//.test(notes)) heroRelease.href = notes;
          heroRelease.hidden = false;
        }
        if (!releaseLine) return;

        var text = 'Latest release ';
        var published = release.published_at ? new Date(release.published_at) : null;

        releaseLine.textContent = '';
        releaseLine.appendChild(document.createTextNode(text));

        var strong = document.createElement('span');
        strong.className = 'tag';
        strong.textContent = tag;
        releaseLine.appendChild(strong);

        if (published && !isNaN(published.getTime())) {
          releaseLine.appendChild(
            document.createTextNode(
              ' · ' +
                published.toLocaleDateString('en-GB', {
                  day: 'numeric', month: 'long', year: 'numeric'
                })
            )
          );
        }
      })
      .catch(function () {
        if (releaseLine) releaseLine.textContent = releaseLine.getAttribute('data-fallback') || '';
      });
  }

  /* ---------------------------------------------------------------
     The year in the footer, so nobody has to remember to change it.
     --------------------------------------------------------------- */
  var year = $('year');
  if (year) year.textContent = String(new Date().getFullYear());

  onScroll();
})();
