/*
  Corvus GCS project website.

  Four small things, and nothing that the page depends on to be readable:
  the header's scrolled state and the mobile menu, a fade-in on first sight,
  the screenshot lightbox, and one optional call to the GitHub releases API.
  If that call fails the download section keeps the static text it shipped
  with, so the page is complete with no network beyond itself.
*/
(function () {
  'use strict';

  var REPO = 'M-BSquared/CorvusGCS';
  var reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)');

  /* ---------------------------------------------------------------
     Theme. The stored choice wins; otherwise dark, as the page ships.
     The <head> applies it before first paint; this only wires the
     button and keeps the two in step.
     --------------------------------------------------------------- */
  var root = document.documentElement;
  var themeToggle = document.getElementById('theme-toggle');
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
     Header: a dark translucent bar and a hairline once the page moves.
     --------------------------------------------------------------- */
  var header = document.getElementById('site-header');
  var scrollTick = false;

  function onScroll() {
    if (scrollTick) return;
    scrollTick = true;
    window.requestAnimationFrame(function () {
      header.classList.toggle('is-scrolled', window.scrollY > 8);
      scrollTick = false;
    });
  }

  if (header) {
    onScroll();
    window.addEventListener('scroll', onScroll, { passive: true });
  }

  /* ---------------------------------------------------------------
     Mobile navigation.
     --------------------------------------------------------------- */
  var navToggle = document.getElementById('nav-toggle');
  var nav = document.getElementById('site-nav');

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
      if (window.innerWidth > 900) setNav(false);
    });
  }

  /* ---------------------------------------------------------------
     Reveal on scroll. Without IntersectionObserver, or with reduced
     motion asked for, everything is simply shown.
     --------------------------------------------------------------- */
  var revealables = Array.prototype.slice.call(document.querySelectorAll('.reveal'));

  function showAll() {
    revealables.forEach(function (el) { el.classList.add('is-visible'); });
  }

  if (reduceMotion.matches || !('IntersectionObserver' in window)) {
    showAll();
  } else {
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible');
          observer.unobserve(entry.target);
        }
      });
    }, { rootMargin: '0px 0px -8% 0px', threshold: 0.08 });

    revealables.forEach(function (el) { observer.observe(el); });
  }

  reduceMotion.addEventListener('change', function (event) {
    if (event.matches) showAll();
  });

  /* ---------------------------------------------------------------
     The hero screenshot leans by a few pixels as the page scrolls,
     enough to feel attached to the scroll, not enough to notice.
     --------------------------------------------------------------- */
  var heroWindow = document.getElementById('hero-window');
  var heroTick = false;

  function onHeroScroll() {
    if (heroTick) return;
    heroTick = true;
    window.requestAnimationFrame(function () {
      var shift = Math.min(window.scrollY, 420) * 0.028;
      heroWindow.style.transform = 'translate3d(0, ' + shift.toFixed(2) + 'px, 0)';
      heroTick = false;
    });
  }

  if (heroWindow && !reduceMotion.matches && window.innerWidth > 900) {
    window.addEventListener('scroll', onHeroScroll, { passive: true });
  }

  /* ---------------------------------------------------------------
     Lightbox. Opens from any screenshot button, closes on the button,
     Escape or the backdrop, and steps with the arrow keys. Focus goes
     in on open and back to the screenshot that opened it.
     --------------------------------------------------------------- */
  var lightbox = document.getElementById('lightbox');
  var lbImage = document.getElementById('lightbox-image');
  var lbCaption = document.getElementById('lightbox-caption');
  var lbCount = document.getElementById('lightbox-count');
  var lbPrev = document.getElementById('lightbox-prev');
  var lbNext = document.getElementById('lightbox-next');
  var lbClose = document.getElementById('lightbox-close');
  var shots = Array.prototype.slice.call(document.querySelectorAll('.shot-btn'));
  var openerIndex = 0;
  var opener = null;

  function preload(index) {
    var total = shots.length;
    var btn = shots[(index + total) % total];
    if (btn) new Image().src = btn.getAttribute('data-full');
  }

  function show(index) {
    var total = shots.length;
    openerIndex = (index + total) % total;
    var btn = shots[openerIndex];
    var img = btn.querySelector('img');
    var figure = btn.closest('figure');
    var caption = figure && figure.querySelector('figcaption');
    var group = btn.closest('[data-group]');

    lbImage.src = btn.getAttribute('data-full');
    lbImage.alt = img ? img.alt : '';
    /* The figure's own caption, title included, so each one is written once. */
    lbCaption.textContent = '';
    if (caption) {
      Array.prototype.forEach.call(caption.childNodes, function (node) {
        lbCaption.appendChild(node.cloneNode(true));
      });
    }
    lbCount.textContent = (group ? group.getAttribute('data-group') + ' · ' : '') +
      (openerIndex + 1) + ' / ' + total;
    /* Stepping through should not wait on the network each time. */
    preload(openerIndex + 1);
    preload(openerIndex - 1);
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

  shots.forEach(function (btn, index) {
    btn.addEventListener('click', function () { openLightbox(index, btn); });
  });

  if (lightbox) {
    lbPrev.addEventListener('click', function () { show(openerIndex - 1); });
    lbNext.addEventListener('click', function () { show(openerIndex + 1); });

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
      if (event.key === 'ArrowLeft') { event.preventDefault(); show(openerIndex - 1); return; }
      if (event.key === 'ArrowRight') { event.preventDefault(); show(openerIndex + 1); return; }

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
     Latest release. Shown only when GitHub answers with a real tag;
     otherwise the line keeps the text it was served with.
     --------------------------------------------------------------- */
  var releaseLine = document.getElementById('release-line');

  if (releaseLine && 'fetch' in window) {
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
        releaseLine.textContent = releaseLine.getAttribute('data-fallback') || '';
      });
  }

  /* ---------------------------------------------------------------
     The year in the footer, so nobody has to remember to change it.
     --------------------------------------------------------------- */
  var year = document.getElementById('year');
  if (year) year.textContent = String(new Date().getFullYear());
})();
