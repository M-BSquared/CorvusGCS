/*
  Corvus GCS documentation: two small things on top of ../script.js, which
  already runs the theme, the header and the site menu.

  The contents button that folds the page list away on a narrow screen, and
  the "On this page" entry for the section being read. Without script the
  list simply stays open and the contents are plain links.
*/
(function () {
  'use strict';

  var nav = document.getElementById('guide-nav');
  var toggle = document.getElementById('guide-nav-toggle');

  if (nav && toggle) {
    toggle.addEventListener('click', function () {
      var open = toggle.getAttribute('aria-expanded') !== 'true';
      toggle.setAttribute('aria-expanded', String(open));
      nav.classList.toggle('is-open', open);
    });
  }

  var links = Array.prototype.slice.call(document.querySelectorAll('.guide-toc a[href^="#"]'));
  if (!links.length || !('IntersectionObserver' in window)) return;

  var byId = {};
  var sections = [];
  links.forEach(function (link) {
    var id = decodeURIComponent(link.getAttribute('href').slice(1));
    var target = document.getElementById(id);
    if (!target) return;
    byId[id] = link;
    sections.push(target);
  });

  var visible = {};

  function mark() {
    /* The first heading still on screen, or failing that the last one passed. */
    var current = null;
    for (var i = 0; i < sections.length; i++) {
      if (visible[sections[i].id]) { current = sections[i]; break; }
      if (sections[i].getBoundingClientRect().top < 120) current = sections[i];
    }
    links.forEach(function (link) { link.classList.remove('is-current'); });
    if (current && byId[current.id]) byId[current.id].classList.add('is-current');
  }

  var observer = new IntersectionObserver(function (entries) {
    entries.forEach(function (entry) { visible[entry.target.id] = entry.isIntersecting; });
    mark();
  }, { rootMargin: '-64px 0px -55% 0px', threshold: 0 });

  sections.forEach(function (section) { observer.observe(section); });
})();
