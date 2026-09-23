// Replays the static terminal transcript as a live session.
// Everything it animates is already in the DOM: with no JavaScript, or with
// prefers-reduced-motion, the reader sees the whole transcript at once.

(function () {
  'use strict';

  var TYPE_MS = 28;
  var TYPE_JITTER_MS = 8;
  var LINE_MS = 14;
  var SPIN_MS = 80;
  var STEP_PAUSE_MS = 700;
  var SPINNER = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';

  var body = document.getElementById('term-body');
  var controls = document.getElementById('term-controls');
  if (!body || !controls) return;

  var steps = Array.prototype.slice.call(body.querySelectorAll('.term-step'));
  if (!steps.length) return;

  var reduced = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (reduced) return;

  var lineHeight = parseFloat(getComputedStyle(body).lineHeight) || 21;
  var tallest = steps.reduce(function (max, step) {
    return Math.max(max, step.getBoundingClientRect().height);
  }, 0);
  body.style.minHeight = Math.ceil(tallest) + 40 + 'px';

  var cursor = document.createElement('span');
  cursor.className = 'cursor';
  cursor.setAttribute('aria-hidden', 'true');

  // The markup separates the steps with newlines so the no-script transcript
  // reads as one session. Those text nodes keep rendering while their step is
  // hidden, which is what stacked a blank line per phase at the top.
  Array.prototype.slice.call(body.childNodes).forEach(function (node) {
    if (node.nodeType === 3 && !node.textContent.trim()) body.removeChild(node);
  });
  body.classList.add('is-live');

  var plan = steps.map(function (step) {
    var out = step.querySelector('.out');
    var cmd = step.querySelector('.cmd');
    var gap = out.previousSibling;
    if (gap && gap.nodeType === 3) gap.textContent = gap.textContent.replace(/\n$/, '');
    out.style.display = 'block';
    out.style.overflow = 'hidden';
    return {
      el: step,
      cmd: cmd,
      out: out,
      text: cmd.textContent,
      lines: out.textContent.split('\n').length,
      work: parseInt(step.dataset.work, 10) || 1000,
      status: step.dataset.status || ''
    };
  });

  var timer = null;
  var paused = false;
  var running = false;

  function stop() {
    if (timer) clearTimeout(timer);
    timer = null;
    running = false;
  }

  function wait(ms, next) {
    timer = setTimeout(function () {
      if (paused) { wait(120, next); return; }
      next();
    }, ms);
  }

  function reset() {
    stop();
    if (cursor.parentNode) cursor.parentNode.removeChild(cursor);
    plan.forEach(function (s) {
      s.el.style.display = 'none';
      s.cmd.textContent = s.text;
      s.out.style.maxHeight = '';
      var status = s.el.querySelector('.status');
      if (status) status.parentNode.removeChild(status);
    });
  }

  function show(step, complete) {
    plan.forEach(function (other) { if (other !== step) other.el.style.display = 'none'; });
    step.el.style.display = '';
    step.cmd.textContent = complete ? step.text : '';
    step.out.style.maxHeight = complete ? 'none' : '0px';
  }

  function typeCommand(step, done) {
    var i = 0;
    (function tick() {
      if (i >= step.text.length) { done(); return; }
      i += 1;
      step.cmd.textContent = step.text.slice(0, i);
      wait(TYPE_MS + Math.round((Math.random() - 0.5) * 2 * TYPE_JITTER_MS), tick);
    })();
  }

  function work(step, done) {
    var status = document.createElement('span');
    status.className = 'status dim';
    step.el.insertBefore(status, step.out.previousSibling || step.out);
    var frame = 0;
    var elapsed = 0;
    (function tick() {
      if (elapsed >= step.work) {
        status.parentNode.removeChild(status);
        done();
        return;
      }
      status.textContent = '\n' + SPINNER.charAt(frame % SPINNER.length) + (step.status ? ' ' + step.status : '');
      frame += 1;
      elapsed += SPIN_MS;
      wait(SPIN_MS, tick);
    })();
  }

  function stream(step, done) {
    var shown = 0;
    (function tick() {
      if (shown >= step.lines) { step.out.style.maxHeight = 'none'; done(); return; }
      shown += 1;
      step.out.style.maxHeight = shown * lineHeight + 'px';
      wait(LINE_MS, tick);
    })();
  }

  function play(index) {
    if (index >= plan.length) { running = false; return; }
    var step = plan[index];
    show(step, false);
    step.cmd.parentNode.appendChild(cursor);
    typeCommand(step, function () {
      work(step, function () {
        if (cursor.parentNode) cursor.parentNode.removeChild(cursor);
        stream(step, function () {
          step.out.appendChild(cursor);
          wait(STEP_PAUSE_MS, function () { play(index + 1); });
        });
      });
    });
  }

  function start() {
    reset();
    running = true;
    press(-1);
    play(0);
  }

  function press(index) {
    chips.forEach(function (chip, i) {
      chip.setAttribute('aria-pressed', String(i === index));
    });
  }

  function jump(index) {
    reset();
    show(plan[index], true);
    press(index);
  }

  var chips = Array.prototype.slice.call(controls.querySelectorAll('[data-jump]'));
  chips.forEach(function (chip, i) {
    chip.addEventListener('click', function () { jump(i); });
  });
  controls.querySelector('[data-replay]').addEventListener('click', start);
  controls.hidden = false;

  document.addEventListener('visibilitychange', function () {
    paused = document.hidden && running;
  });

  reset();
  show(plan[0], false);

  if (!('IntersectionObserver' in window)) { start(); return; }
  var observer = new IntersectionObserver(function (entries) {
    entries.forEach(function (entry) {
      if (entry.isIntersecting) { observer.disconnect(); start(); }
    });
  }, { threshold: 0.5 });
  observer.observe(document.getElementById('term'));
})();
