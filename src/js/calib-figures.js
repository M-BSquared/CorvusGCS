"use strict";
window.Corvus = window.Corvus || {};

/**
 * Corvus.calibFigures — the aircraft attitude figures used by the calibration
 * wizard.
 *
 * The operator's real question during a sensor calibration is physical: "which
 * way do I have to hold the aircraft, and which way do I turn it?" A word like
 * PX4's "back side" does not answer that. So each step is drawn: a raven —
 * Corvus, wings spread — rendered in the exact attitude PX4 is asking for,
 * standing on a ground plane so "down" is unambiguous.
 *
 * The figure is a real (small) 3-D pipeline rather than six hand-drawn sprites:
 * one model, rotated by the pose matrix, orthographically projected, painter-
 * sorted and Lambert-shaded. That is what makes the six accelerometer sides and
 * the compass rotation consistent with each other — they are literally the same
 * bird seen from the same camera — and it is what lets the compass figure spin
 * about the world vertical, which is the motion PX4 actually wants.
 *
 * Body frame: +X nose, +Y right wing, +Z up.
 *
 * Exposes:
 *   POSES                      the six PX4 accelerometer orientations
 *   poseLabel/poseHint(pose)   operator-facing strings for a pose
 *   create(host, opts)         -> { el, set(opts), destroy() }
 *
 * Degrades to a labelled placeholder where `createElementNS` is unavailable
 * (the DOM-stub test harness), so the wizard never depends on SVG existing.
 */
Corvus.calibFigures = (function () {
  const SVG_NS = "http://www.w3.org/2000/svg";

  // --- camera ---------------------------------------------------------------
  // A fixed front-left-above three-quarter view. Fixed on purpose: the whole
  // point of the figures is that six attitudes are comparable, which they stop
  // being the moment the camera moves too.

  const AZ = 35 * Math.PI / 180;
  const EL = 24 * Math.PI / 180;

  const CAM = (function () {
    const f = [
      Math.cos(EL) * Math.cos(AZ),
      Math.cos(EL) * Math.sin(AZ),
      Math.sin(EL),
    ];

    const rx = -f[1];
    const ry = f[0];
    const rl = Math.hypot(rx, ry) || 1;
    const r = [rx / rl, ry / rl, 0];

    const u = [
      f[1] * r[2] - f[2] * r[1],
      f[2] * r[0] - f[0] * r[2],
      f[0] * r[1] - f[1] * r[0],
    ];

    return { f, r, u };
  })();

  // --- pose matrices --------------------------------------------------------

  function rotX(a) {
    const c = Math.cos(a);
    const s = Math.sin(a);

    return [
      [1, 0, 0],
      [0, c, -s],
      [0, s, c],
    ];
  }

  function rotY(a) {
    const c = Math.cos(a);
    const s = Math.sin(a);

    return [
      [c, 0, s],
      [0, 1, 0],
      [-s, 0, c],
    ];
  }

  function rotZ(a) {
    const c = Math.cos(a);
    const s = Math.sin(a);

    return [
      [c, -s, 0],
      [s, c, 0],
      [0, 0, 1],
    ];
  }

  function mul(a, b) {
    const m = [
      [0, 0, 0],
      [0, 0, 0],
      [0, 0, 0],
    ];

    for (let i = 0; i < 3; i++) {
      for (let j = 0; j < 3; j++) {
        m[i][j] =
          a[i][0] * b[0][j] +
          a[i][1] * b[1][j] +
          a[i][2] * b[2][j];
      }
    }

    return m;
  }

  function apply(m, v) {
    return [
      m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
      m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
      m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ];
  }

  const HALF = Math.PI / 2;

  /* The six PX4 accelerometer orientations, keyed by our own names. PX4's own
     words for them ("back", "front", "up", "down") are mapped in
     Corvus.calibProtocol — this module only speaks attitudes. */
  const POSE_M = {
    level: [
      [1, 0, 0],
      [0, 1, 0],
      [0, 0, 1],
    ],
    nose_down: rotY(HALF),
    tail_down: rotY(-HALF),
    left: rotX(HALF),
    right: rotX(-HALF),
    upside_down: rotX(Math.PI),
  };

  const POSES = [
    "level",
    "nose_down",
    "tail_down",
    "left",
    "right",
    "upside_down",
  ];

  const POSE_LABEL = {
    level: "Level",
    nose_down: "Nose down",
    tail_down: "Tail down",
    left: "Left side down",
    right: "Right side down",
    upside_down: "Upside down",
  };

  const POSE_HINT = {
    level: "Wings level, nose forward — flat on the surface.",
    nose_down: "Stand it on its nose, tail straight up.",
    tail_down: "Stand it on its tail, nose straight up.",
    left: "Roll it onto its left wing.",
    right: "Roll it onto its right wing.",
    upside_down: "Turn it over — belly up, nose still forward.",
  };

  function poseLabel(pose) {
    return POSE_LABEL[pose] || "Level";
  }

  function poseHint(pose) {
    return POSE_HINT[pose] || POSE_HINT.level;
  }

  // --- raven model ----------------------------------------------------------

  const SLATE = "#252C37";
  const BEAK = "#E8842E";
  const BEAK_TIP = "#C96620";
  const EYE = "#FFD58A";

  function station(x, halfWidth, top, bottom) {
    return section(x, halfWidth, 0, top, -bottom);
  }

  function section(x, halfWidth, sideZ, topZ, bottomZ) {
    const points = [];

    for (let i = 0; i < 16; i++) {
      const angle = (i / 16) * Math.PI * 2;
      const sine = Math.sin(angle);

      const z =
        sine >= 0
          ? sideZ + sine * (topZ - sideZ)
          : sideZ + -sine * (bottomZ - sideZ);

      points.push([
        x,
        Math.cos(angle) * halfWidth,
        z,
      ]);
    }

    return points;
  }

  function buildModel() {
    const verts = [];
    const faces = [];

    const push = (point) => {
      verts.push(point);
      return verts.length - 1;
    };

    // Body, chest, neck and head.

    const stations = [
      station(-0.94, 0.060, 0.070, 0.080),
      station(-0.78, 0.115, 0.125, 0.140),
      station(-0.60, 0.180, 0.200, 0.215),
      station(-0.38, 0.245, 0.275, 0.285),
      station(-0.15, 0.295, 0.330, 0.320),
      station(0.08, 0.320, 0.360, 0.330),
      station(0.28, 0.315, 0.350, 0.310),
      station(0.47, 0.285, 0.330, 0.260),
      station(0.62, 0.300, 0.360, 0.210),
      station(0.76, 0.300, 0.355, 0.180),
      station(0.89, 0.270, 0.310, 0.150),
      station(0.99, 0.205, 0.230, 0.120),
    ];

    const stationIndices = stations.map((currentStation) =>
      currentStation.map(push)
    );

    for (let s = 0; s < stationIndices.length - 1; s++) {
      for (let k = 0; k < stationIndices[s].length; k++) {
        const next = (k + 1) % stationIndices[s].length;

        faces.push({
          v: [
            stationIndices[s][k],
            stationIndices[s][next],
            stationIndices[s + 1][next],
            stationIndices[s + 1][k],
          ],
          role: s < 7 ? "body" : "head",
        });
      }
    }

    faces.push({
      v: stationIndices[0].slice().reverse(),
      role: "body",
    });

    // Orange, gently curved beak.

    const beakSections = [
      section(0.88, 0.180, 0.095, 0.220, 0.020),
      section(0.98, 0.165, 0.082, 0.190, 0.010),
      section(1.08, 0.145, 0.065, 0.155, -0.005),
      section(1.19, 0.115, 0.043, 0.130, -0.020),
      section(1.30, 0.085, 0.020, 0.100, -0.035),
      section(1.38, 0.050, -0.005, 0.060, -0.055),
      section(1.43, 0.020, -0.025, 0.020, -0.075),
    ];

    const beakIndices = beakSections.map((currentSection) =>
      currentSection.map(push)
    );

    for (let s = 0; s < beakIndices.length - 1; s++) {
      const role =
        s >= beakIndices.length - 2
          ? "beakTip"
          : "beak";

      for (let k = 0; k < beakIndices[s].length; k++) {
        const next = (k + 1) % beakIndices[s].length;

        faces.push({
          v: [
            beakIndices[s][k],
            beakIndices[s][next],
            beakIndices[s + 1][next],
            beakIndices[s + 1][k],
          ],
          role,
        });
      }
    }

    function plate(outline, thickness, role) {
      const top = outline.map((point) =>
        push([
          point[0],
          point[1],
          point[2] + thickness / 2,
        ])
      );

      const bottom = outline.map((point) =>
        push([
          point[0],
          point[1],
          point[2] - thickness / 2,
        ])
      );

      faces.push({
        v: top.slice(),
        role,
      });

      faces.push({
        v: bottom.slice().reverse(),
        role,
      });

      for (let k = 0; k < outline.length; k++) {
        const next = (k + 1) % outline.length;

        faces.push({
          v: [
            top[k],
            top[next],
            bottom[next],
            bottom[k],
          ],
          role,
        });
      }
    }

    function sidePlate(outline, y, thickness, role) {
      const front = outline.map((point) =>
        push([
          point[0],
          y + thickness / 2,
          point[1],
        ])
      );

      const back = outline.map((point) =>
        push([
          point[0],
          y - thickness / 2,
          point[1],
        ])
      );

      faces.push({
        v: front.slice(),
        role,
      });

      faces.push({
        v: back.slice().reverse(),
        role,
      });

      for (let k = 0; k < outline.length; k++) {
        const next = (k + 1) % outline.length;

        faces.push({
          v: [
            front[k],
            front[next],
            back[next],
            back[k],
          ],
          role,
        });
      }
    }

    function ovalSide(
      centerX,
      centerZ,
      radiusX,
      radiusZ,
      y,
      thickness,
      role
    ) {
      const outline = [];

      for (let i = 0; i < 16; i++) {
        const angle = (i / 16) * Math.PI * 2;

        outline.push([
          centerX + Math.cos(angle) * radiusX,
          centerZ + Math.sin(angle) * radiusZ,
        ]);
      }

      sidePlate(outline, y, thickness, role);
    }

    function tube(startPoint, endPoint, startRadius, endRadius, role) {
      const direction = [
        endPoint[0] - startPoint[0],
        endPoint[1] - startPoint[1],
        endPoint[2] - startPoint[2],
      ];

      const directionLength =
        Math.hypot(
          direction[0],
          direction[1],
          direction[2]
        ) || 1;

      direction[0] /= directionLength;
      direction[1] /= directionLength;
      direction[2] /= directionLength;

      const reference =
        Math.abs(direction[2]) < 0.9
          ? [0, 0, 1]
          : [0, 1, 0];

      let u = [
        direction[1] * reference[2] -
          direction[2] * reference[1],
        direction[2] * reference[0] -
          direction[0] * reference[2],
        direction[0] * reference[1] -
          direction[1] * reference[0],
      ];

      const uLength =
        Math.hypot(u[0], u[1], u[2]) || 1;

      u = [
        u[0] / uLength,
        u[1] / uLength,
        u[2] / uLength,
      ];

      const v = [
        direction[1] * u[2] -
          direction[2] * u[1],
        direction[2] * u[0] -
          direction[0] * u[2],
        direction[0] * u[1] -
          direction[1] * u[0],
      ];

      function createRing(center, radius) {
        const result = [];

        for (let i = 0; i < 10; i++) {
          const angle = (i / 10) * Math.PI * 2;
          const cosine = Math.cos(angle);
          const sine = Math.sin(angle);

          result.push(
            push([
              center[0] +
                radius * (cosine * u[0] + sine * v[0]),
              center[1] +
                radius * (cosine * u[1] + sine * v[1]),
              center[2] +
                radius * (cosine * u[2] + sine * v[2]),
            ])
          );
        }

        return result;
      }

      const start = createRing(startPoint, startRadius);
      const end = createRing(endPoint, endRadius);

      faces.push({
        v: start.slice().reverse(),
        role,
      });

      faces.push({
        v: end.slice(),
        role,
      });

      for (let i = 0; i < start.length; i++) {
        const next = (i + 1) % start.length;

        faces.push({
          v: [
            start[i],
            start[next],
            end[next],
            end[i],
          ],
          role,
        });
      }
    }

    // Enlarged wings.

    const WING = [
      [0.49, 0.16],
      [0.49, 0.34],
      [0.46, 0.52],
      [0.40, 0.70],
      [0.32, 0.88],
      [0.21, 1.05],
      [0.08, 1.20],
      [-0.08, 1.34],
      [-0.27, 1.46],
      [-0.35, 1.49],
      [-0.36, 1.40],
      [-0.33, 1.30],
      [-0.29, 1.21],
      [-0.43, 1.27],
      [-0.51, 1.26],
      [-0.51, 1.18],
      [-0.48, 1.10],
      [-0.43, 1.02],
      [-0.57, 1.06],
      [-0.65, 1.05],
      [-0.65, 0.97],
      [-0.61, 0.89],
      [-0.55, 0.80],
      [-0.69, 0.82],
      [-0.76, 0.78],
      [-0.74, 0.71],
      [-0.68, 0.64],
      [-0.58, 0.56],
      [-0.68, 0.52],
      [-0.71, 0.46],
      [-0.67, 0.40],
      [-0.58, 0.35],
      [-0.51, 0.22],
      [-0.48, 0.17],
    ];

    const WING_SPAN = 1.14;
    const WING_CHORD = 1.06;

    function growWing(point) {
      const result = [
        point[0] * WING_CHORD,
        0.16 + (point[1] - 0.16) * WING_SPAN,
      ];

      if (point.length > 2) {
        result.push(point[2]);
      }

      return result;
    }

    const wingOutline = WING.map(growWing);

    function dihedral(y) {
      return (
        0.07 +
        0.15 *
          ((Math.abs(y) - 0.16) / 1.52)
      );
    }

    plate(
      wingOutline.map((point) => [
        point[0],
        point[1],
        dihedral(point[1]),
      ]),
      0.050,
      "wing"
    );

    plate(
      wingOutline
        .map((point) => [
          point[0],
          -point[1],
          dihedral(point[1]),
        ])
        .reverse(),
      0.050,
      "wing"
    );

    function mirroredWingPlate(outline, thickness, role) {
      const grown = outline.map(growWing);

      plate(grown, thickness, role);

      plate(
        grown
          .map((point) => [
            point[0],
            -point[1],
            point[2],
          ])
          .reverse(),
        thickness,
        role
      );
    }

    mirroredWingPlate(
      [
        [0.43, 0.18, 0.125],
        [0.38, 0.40, 0.150],
        [0.26, 0.61, 0.175],
        [0.08, 0.82, 0.195],
        [-0.13, 1.01, 0.215],
        [-0.26, 1.13, 0.225],
        [-0.35, 1.04, 0.214],
        [-0.30, 0.85, 0.195],
        [-0.17, 0.64, 0.166],
        [-0.38, 0.32, 0.118],
        [-0.36, 0.18, 0.105],
      ],
      0.028,
      "wingLayer"
    );

    // Tail.

    const TAIL = [
      [-0.82, 0.16, 0.02],
      [-1.04, 0.36, 0.01],
      [-1.23, 0.44, -0.01],
      [-1.43, 0.40, -0.03],
      [-1.49, 0.34, -0.035],
      [-1.38, 0.22, -0.03],
      [-1.34, 0.16, -0.03],
      [-1.62, 0.06, -0.05],
      [-1.68, 0.00, -0.055],
      [-1.62, -0.06, -0.05],
      [-1.34, -0.16, -0.03],
      [-1.38, -0.22, -0.03],
      [-1.49, -0.34, -0.035],
      [-1.43, -0.40, -0.03],
      [-1.23, -0.44, -0.01],
      [-1.04, -0.36, 0.01],
      [-0.82, -0.16, 0.02],
    ];

    plate(TAIL, 0.04, "tail");

    plate(
      [
        [-0.80, 0.12, 0.065],
        [-1.23, 0.20, 0.055],
        [-1.61, 0.00, 0.005],
        [-1.23, -0.20, 0.055],
        [-0.80, -0.12, 0.065],
      ],
      0.025,
      "tailLayer"
    );

    // Soft crown feathers.

    sidePlate(
      [
        [0.48, 0.30],
        [0.57, 0.395],
        [0.66, 0.37],
        [0.75, 0.405],
        [0.86, 0.30],
      ],
      0,
      0.16,
      "crest"
    );

    // Legs, talons, eyes and expression.

    [-1, 1].forEach((side) => {
      tube(
        [0.00, side * 0.15, -0.28],
        [0.02, side * 0.15, -0.49],
        0.045,
        0.033,
        "leg"
      );

      tube(
        [0.02, side * 0.15, -0.49],
        [0.27, side * 0.12, -0.52],
        0.034,
        0.014,
        "talon"
      );

      tube(
        [0.03, side * 0.15, -0.49],
        [0.23, side * 0.21, -0.52],
        0.030,
        0.012,
        "talon"
      );

      tube(
        [0.00, side * 0.15, -0.49],
        [-0.13, side * 0.16, -0.51],
        0.028,
        0.012,
        "talon"
      );

      ovalSide(
        0.80,
        0.155,
        0.105,
        0.084,
        side * 0.302,
        0.032,
        "eye"
      );

      ovalSide(
        0.835,
        0.148,
        0.040,
        0.050,
        side * 0.323,
        0.012,
        "pupil"
      );

      ovalSide(
        0.820,
        0.176,
        0.014,
        0.018,
        side * 0.333,
        0.007,
        "eyeHighlight"
      );

      sidePlate(
        [
          [0.68, 0.235],
          [0.79, 0.260],
          [0.91, 0.245],
          [0.90, 0.215],
          [0.79, 0.228],
          [0.70, 0.212],
        ],
        side * 0.309,
        0.026,
        "brow"
      );

      const beakSeam = [
        push([0.92, side * 0.174, 0.080]),
        push([1.31, side * 0.080, 0.020]),
        push([1.34, side * 0.072, -0.004]),
        push([0.93, side * 0.168, 0.052]),
      ];

      faces.push({
        v:
          side > 0
            ? beakSeam
            : beakSeam.slice().reverse(),
        role: "beakLine",
      });
    });

    return { verts, faces };
  }

  const MODEL = buildModel();

  /** Radius of the model's bounding sphere — the fixed scale reference, so no
   *  attitude ever renders bigger or smaller than the others. */
  const MODEL_R = MODEL.verts.reduce(
    (maximum, point) =>
      Math.max(
        maximum,
        Math.hypot(point[0], point[1], point[2])
      ),
    0
  );

  // --- shading --------------------------------------------------------------

  const LIGHT = (function () {
    const vector = [-0.32, 0.52, 0.79];

    const length = Math.hypot(
      vector[0],
      vector[1],
      vector[2]
    );

    return [
      vector[0] / length,
      vector[1] / length,
      vector[2] / length,
    ];
  })();

  const ROLE_COLOR = {
    body: SLATE,
    head: "#303844",
    wing: "#1F252E",
    wingLayer: "#343E4B",
    tail: "#1F252E",
    tailLayer: "#303946",
    crest: "#303946",
    beak: BEAK,
    beakTip: BEAK_TIP,
    beakLine: "#7F3A16",
    eye: EYE,
    pupil: "#10141C",
    eyeHighlight: "#FFF9E8",
    brow: "#28313D",
    leg: "#464A4F",
    talon: "#2C3035",
  };

  function shade(hex, amount) {
    const value = parseInt(hex.slice(1), 16);

    const clamp = (number) =>
      Math.max(
        0,
        Math.min(255, Math.round(number))
      );

    const red = clamp(
      ((value >> 16) & 255) * amount
    );

    const green = clamp(
      ((value >> 8) & 255) * amount
    );

    const blue = clamp(
      (value & 255) * amount
    );

    return `rgb(${red},${green},${blue})`;
  }

  function faceNormal(a, b, c) {
    const u = [
      b[0] - a[0],
      b[1] - a[1],
      b[2] - a[2],
    ];

    const v = [
      c[0] - a[0],
      c[1] - a[1],
      c[2] - a[2],
    ];

    const normal = [
      u[1] * v[2] - u[2] * v[1],
      u[2] * v[0] - u[0] * v[2],
      u[0] * v[1] - u[1] * v[0],
    ];

    const length =
      Math.hypot(
        normal[0],
        normal[1],
        normal[2]
      ) || 1;

    return [
      normal[0] / length,
      normal[1] / length,
      normal[2] / length,
    ];
  }

  // --- SVG helpers ----------------------------------------------------------

  function svgSupported() {
    return (
      typeof document !== "undefined" &&
      typeof document.createElementNS === "function"
    );
  }

  function node(name, attrs) {
    const element = document.createElementNS(
      SVG_NS,
      name
    );

    if (attrs) {
      for (const key of Object.keys(attrs)) {
        element.setAttribute(
          key,
          String(attrs[key])
        );
      }
    }

    return element;
  }

  function pts(list) {
    return list
      .map(
        (point) =>
          `${point[0].toFixed(1)},${point[1].toFixed(1)}`
      )
      .join(" ");
  }

  // --- figure creation ------------------------------------------------------

  /**
   * Build a figure inside `host`.
   *
   * @param {HTMLElement} host
   * @param {{compact?:boolean, pose?:string, spin?:boolean, marker?:string,
   *          reduced?:boolean}} [opts]
   * @returns {{el:Object, set:function, destroy:function}} `set` accepts
   *   `{pose, spin, marker}`; `destroy` stops the animation frame. Both are
   *   idempotent and safe after destroy.
   */
  function create(host, opts) {
    const options = opts || {};
    const compact = !!options.compact;

    const view = compact
      ? { w: 128, h: 100 }
      : { w: 240, h: 178 };

    const scale =
      (compact ? 46 : 78) / MODEL_R;

    const centerX = view.w / 2;
    const centerY = compact
      ? view.h / 2 - 4
      : 74;

    const groundZ = -MODEL_R * 1.06;

    let pose =
      options.pose && POSE_M[options.pose]
        ? options.pose
        : "level";

    let spin = !!options.spin;
    let marker = options.marker || null;
    let animationFrame = 0;
    let destroyed = false;
    let angle = 0;
    let previousTime = 0;

    if (!svgSupported()) {
      // Test/no-SVG fallback: still announce the attitude so the wizard's
      // instruction is complete without the drawing.
      const placeholder =
        document.createElement("div");

      placeholder.className =
        "calib-figure calib-figure-fallback";

      placeholder.dataset.pose = pose;
      placeholder.textContent = poseLabel(pose);

      host.appendChild(placeholder);

      return {
        el: placeholder,

        set(next) {
          if (destroyed || !next) {
            return;
          }

          if (next.pose && POSE_M[next.pose]) {
            pose = next.pose;
          }

          if (typeof next.spin === "boolean") {
            spin = next.spin;
          }

          placeholder.dataset.pose = pose;
          placeholder.dataset.spin =
            spin ? "1" : "0";

          placeholder.textContent =
            poseLabel(pose);
        },

        destroy() {
          destroyed = true;
        },
      };
    }

    const svg = node("svg", {
      viewBox: `0 0 ${view.w} ${view.h}`,
      class:
        "calib-figure" +
        (compact
          ? " calib-figure-compact"
          : ""),
      "shape-rendering": "geometricPrecision",
      role: "img",
      "aria-label": poseLabel(pose),
    });

    const groundGroup = node("g", {
      class: "calib-figure-ground",
    });

    const backArrowGroup = node("g", {
      class: "calib-figure-arrow",
    });

    const bodyGroup = node("g", {
      class: "calib-figure-body",
    });

    const frontArrowGroup = node("g", {
      class: "calib-figure-arrow",
    });

    const markerGroup = node("g", {
      class: "calib-figure-marker",
    });

    if (!compact) {
      svg.appendChild(groundGroup);
    }

    svg.appendChild(backArrowGroup);
    svg.appendChild(bodyGroup);
    svg.appendChild(frontArrowGroup);
    svg.appendChild(markerGroup);

    host.appendChild(svg);

    // One polygon per face, reused across frames — the spin animation only
    // rewrites `points` and `fill`, so it never churns the DOM.
    const polygons = MODEL.faces.map(() => {
      const polygon = node("polygon", {
        stroke: "none",
        "stroke-linejoin": "round",
      });

      bodyGroup.appendChild(polygon);

      return polygon;
    });

    // Last fill written per polygon. The model has enough faces that skipping
    // the unchanged ones is worth the bookkeeping on a spinning figure.
    const lastFill = new Array(polygons.length).fill("");

    function project(point) {
      return [
        centerX +
          scale *
            (
              point[0] * CAM.r[0] +
              point[1] * CAM.r[1] +
              point[2] * CAM.r[2]
            ),

        centerY -
          scale *
            (
              point[0] * CAM.u[0] +
              point[1] * CAM.u[1] +
              point[2] * CAM.u[2]
            ),
      ];
    }

    function depth(point) {
      return (
        point[0] * CAM.f[0] +
        point[1] * CAM.f[1] +
        point[2] * CAM.f[2]
      );
    }

    function drawGround() {
      if (compact) {
        return;
      }

      while (groundGroup.firstChild) {
        groundGroup.removeChild(
          groundGroup.firstChild
        );
      }

      const radius = MODEL_R * 0.95;
      const divisions = 4;

      for (let i = 0; i <= divisions; i++) {
        const value =
          -radius +
          (2 * radius * i) / divisions;

        const lineAStart = project([
          value,
          -radius,
          groundZ,
        ]);

        const lineAEnd = project([
          value,
          radius,
          groundZ,
        ]);

        groundGroup.appendChild(
          node("line", {
            x1: lineAStart[0],
            y1: lineAStart[1],
            x2: lineAEnd[0],
            y2: lineAEnd[1],
          })
        );

        const lineBStart = project([
          -radius,
          value,
          groundZ,
        ]);

        const lineBEnd = project([
          radius,
          value,
          groundZ,
        ]);

        groundGroup.appendChild(
          node("line", {
            x1: lineBStart[0],
            y1: lineBStart[1],
            x2: lineBEnd[0],
            y2: lineBEnd[1],
          })
        );
      }

      const shadowRing = [];

      for (let i = 0; i < 28; i++) {
        const shadowAngle =
          (i / 28) * Math.PI * 2;

        shadowRing.push(
          project([
            Math.cos(shadowAngle) *
              MODEL_R *
              0.6,
            Math.sin(shadowAngle) *
              MODEL_R *
              0.6,
            groundZ,
          ])
        );
      }

      groundGroup.appendChild(
        node("polygon", {
          class: "calib-figure-shadow",
          points: pts(shadowRing),
        })
      );
    }

    function drawArrow() {
      while (backArrowGroup.firstChild) {
        backArrowGroup.removeChild(
          backArrowGroup.firstChild
        );
      }

      while (frontArrowGroup.firstChild) {
        frontArrowGroup.removeChild(
          frontArrowGroup.firstChild
        );
      }

      if (!spin) {
        return;
      }

      // The rotation PX4 asks for during a compass calibration is about the
      // world vertical while the aircraft is held in the current attitude — so
      // the guide ring is a world-frame circle, not a body-frame one.
      const radius = MODEL_R * 0.98;
      const divisions = 48;
      const ring = [];

      for (let i = 0; i <= divisions; i++) {
        const ringAngle =
          (i / divisions) * Math.PI * 2;

        ring.push([
          Math.cos(ringAngle) * radius,
          Math.sin(ringAngle) * radius,
          -0.15,
        ]);
      }

      // Split at the horizon so the near half draws over the bird and the far
      // half behind it — without that the ring reads as a flat circle on top.
      let run = [];
      let runBehind = depth(ring[0]) < 0;

      function flush() {
        if (run.length > 1) {
          const destination = runBehind
            ? backArrowGroup
            : frontArrowGroup;

          destination.appendChild(
            node("polyline", {
              points: pts(run.map(project)),
              fill: "none",
            })
          );
        }

        run = [];
      }

      ring.forEach((point) => {
        const behind = depth(point) < 0;

        if (
          behind !== runBehind &&
          run.length
        ) {
          run.push(point);
          flush();
          runBehind = behind;
        }

        run.push(point);
      });

      flush();

      // Head, placed on the near side and aimed along the tangent.
      const headAngle = Math.PI * 0.5;

      const tip = [
        Math.cos(headAngle) * radius,
        Math.sin(headAngle) * radius,
        -0.15,
      ];

      const tangent = [
        -Math.sin(headAngle),
        Math.cos(headAngle),
        0,
      ];

      const radial = [
        Math.cos(headAngle),
        Math.sin(headAngle),
        0,
      ];

      const arrowHead = [
        project([
          tip[0] + tangent[0] * 0.3,
          tip[1] + tangent[1] * 0.3,
          tip[2],
        ]),

        project([
          tip[0] -
            tangent[0] * 0.12 +
            radial[0] * 0.16,
          tip[1] -
            tangent[1] * 0.12 +
            radial[1] * 0.16,
          tip[2],
        ]),

        project([
          tip[0] -
            tangent[0] * 0.12 -
            radial[0] * 0.16,
          tip[1] -
            tangent[1] * 0.12 -
            radial[1] * 0.16,
          tip[2],
        ]),
      ];

      const destination =
        depth(tip) < 0
          ? backArrowGroup
          : frontArrowGroup;

      destination.appendChild(
        node("polygon", {
          class:
            "calib-figure-arrowhead",
          points: pts(arrowHead),
        })
      );
    }

    function drawMarker(matrix) {
      while (markerGroup.firstChild) {
        markerGroup.removeChild(
          markerGroup.firstChild
        );
      }

      if (marker !== "nose") {
        return;
      }

      const projectedPosition = project(
        apply(matrix, [
          1.42,
          0,
          -0.040,
        ])
      );

      markerGroup.appendChild(
        node("circle", {
          cx: projectedPosition[0],
          cy: projectedPosition[1],
          r: compact ? 3 : 5,
        })
      );
    }

    function draw() {
      const matrix = spin
        ? mul(rotZ(angle), POSE_M[pose])
        : POSE_M[pose];

      const world = MODEL.verts.map(
        (vertex) => apply(matrix, vertex)
      );

      const order = MODEL.faces
        .map((face, index) => {
          let averageDepth = 0;

          for (const vertexIndex of face.v) {
            averageDepth += depth(
              world[vertexIndex]
            );
          }

          return {
            index,
            depth:
              averageDepth / face.v.length,
          };
        })
        .sort(
          (a, b) => a.depth - b.depth
        );

      // Painter's algorithm: far faces first. Every polygon element is reused;
      // the sort only decides which element ends up where in the DOM order.
      order.forEach((entry, slot) => {
        const face =
          MODEL.faces[entry.index];

        const polygon = polygons[slot];

        const points3D = face.v.map(
          (vertexIndex) =>
            world[vertexIndex]
        );

        const normal = faceNormal(
          points3D[0],
          points3D[1],
          points3D[2]
        );

        const lambert = Math.max(
          0,
          normal[0] * LIGHT[0] +
            normal[1] * LIGHT[1] +
            normal[2] * LIGHT[2]
        );

        const baseColor =
          ROLE_COLOR[face.role] || SLATE;

        polygon.setAttribute(
          "points",
          pts(points3D.map(project))
        );

        const fill = shade(
          baseColor,
          0.86 + 0.18 * lambert
        );

        if (lastFill[slot] !== fill) {
          lastFill[slot] = fill;
          polygon.setAttribute("fill", fill);
        }

        if (
          polygon.parentNode !== bodyGroup ||
          bodyGroup.childNodes[slot] !== polygon
        ) {
          bodyGroup.appendChild(polygon);
        }
      });

      drawArrow();
      drawMarker(matrix);

      svg.setAttribute(
        "aria-label",
        poseLabel(pose) +
          (spin ? ", rotating" : "")
      );
    }

    function tick(now) {
      if (destroyed) {
        return;
      }

      const time =
        typeof now === "number"
          ? now
          : 0;

      if (previousTime) {
        angle +=
          ((time - previousTime) / 1000) *
          0.9;
      }

      previousTime = time;

      draw();

      animationFrame =
        window.requestAnimationFrame(tick);
    }

    function stopSpin() {
      if (
        animationFrame &&
        typeof window.cancelAnimationFrame ===
          "function"
      ) {
        window.cancelAnimationFrame(
          animationFrame
        );
      }

      animationFrame = 0;
      previousTime = 0;
    }

    function refresh() {
      stopSpin();

      // Reduced motion keeps the guide ring — the instruction — but not the
      // motion, which is the accessible reading of "show me the rotation".
      if (
        spin &&
        !options.reduced &&
        typeof window !== "undefined" &&
        typeof window.requestAnimationFrame ===
          "function"
      ) {
        animationFrame =
          window.requestAnimationFrame(tick);
      } else {
        draw();
      }
    }

    drawGround();
    refresh();

    return {
      el: svg,

      set(next) {
        if (destroyed || !next) {
          return;
        }

        let changed = false;

        if (
          next.pose &&
          POSE_M[next.pose] &&
          next.pose !== pose
        ) {
          pose = next.pose;
          changed = true;
        }

        if (
          typeof next.spin === "boolean" &&
          next.spin !== spin
        ) {
          spin = next.spin;
          changed = true;
        }

        if (
          "marker" in next &&
          next.marker !== marker
        ) {
          marker = next.marker;
          changed = true;
        }

        if (changed) {
          refresh();
        }
      },

      destroy() {
        destroyed = true;
        stopSpin();
      },
    };
  }

  return {
    POSES,
    POSE_LABEL,
    poseLabel,
    poseHint,
    create,
  };
})();
