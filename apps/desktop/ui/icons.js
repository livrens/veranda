// Veranda icon set - drawn by hand on a 24x24 grid: 1.8px rounded strokes, 2.5px safe margin,
// filled dots only for status accents. Every icon is a plain <svg> string; no external assets.
(function () {
  const G = '<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">';
  const svg = (body, cls = "") => `<svg viewBox="0 0 24 24" aria-hidden="true"${cls ? ` class="${cls}"` : ""}>${G}${body}</g></svg>`;
  const dot = (x, y, r = 1.3) => `<circle cx="${x}" cy="${y}" r="${r}" fill="currentColor" stroke="none"/>`;

  window.VERANDA_ICONS = {
    // ---- navigation
    overview: svg('<path d="M4 10.4 12 4l8 6.4"/><path d="M6.2 9.6V19h11.6V9.6"/><path d="M10 19v-4.6h4V19"/>'),
    sites: svg('<rect x="3" y="4.5" width="18" height="15" rx="2.5"/><path d="M3 9h18"/>' + dot(6.3, 6.8, 0.9) + dot(9, 6.8, 0.9) + '<path d="M7 13h4M7 16h7"/>'),
    database: svg('<ellipse cx="12" cy="6" rx="7" ry="2.6"/><path d="M5 6v12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6V6"/><path d="M5 12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6"/>'),
    logs: svg('<path d="M7 3h7l5 5v13H7z"/><path d="M14 3v5h5"/><path d="M9.5 12h5M9.5 15.5h5M9.5 9h1.5"/>'),
    settings: svg('<path d="M4 7h16M4 12h16M4 17h16"/>' + '<circle cx="9" cy="7" r="2" fill="var(--surface, #fff)"/><circle cx="15" cy="12" r="2" fill="var(--surface, #fff)"/><circle cx="8" cy="17" r="2" fill="var(--surface, #fff)"/>'),

    // ---- services & packages
    web: svg('<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17"/><path d="M12 3.5c2.6 2.4 3.8 5.3 3.8 8.5s-1.2 6.1-3.8 8.5c-2.6-2.4-3.8-5.3-3.8-8.5S9.4 5.9 12 3.5z"/>'),
    php: svg('<rect x="2.5" y="6.5" width="19" height="11" rx="3"/><text x="12" y="14.9" text-anchor="middle" font-family="ui-sans-serif, system-ui, sans-serif" font-size="7" font-weight="800" fill="currentColor" stroke="none" letter-spacing=".2">php</text>'),
    db: svg('<ellipse cx="12" cy="6" rx="7" ry="2.6"/><path d="M5 6v12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6V6"/><path d="M5 12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6"/>'),
    cache: svg('<path d="M13.5 3 6 13.5h5.2L10.5 21 18 10.5h-5.2z"/>'),
    mail: svg('<rect x="3" y="5" width="18" height="14" rx="2.5"/><path d="m4 7.5 8 5.5 8-5.5"/>'),
    pkg: svg('<path d="M12 3 4 7.2v9.6L12 21l8-4.2V7.2z"/><path d="M4.4 7.4 12 11.5l7.6-4.1M12 11.5V21"/>'),
    node: svg('<path d="M12 3 4.5 7.3v8.4L12 20l7.5-4.3V7.3z"/><path d="M12 8.2v7.6M8.9 10l3.1 1.8 3.1-1.8"/>'),
    lang: svg('<path d="m8.5 7.5-5 4.5 5 4.5M15.5 7.5l5 4.5-5 4.5M13.5 5l-3 14"/>'),
    tool: svg('<path d="M14.7 6.3a4 4 0 0 0 5 5L9.6 21.5a2.1 2.1 0 0 1-3-3L16.7 8.4"/><path d="M14.7 6.3 18 3l3 3-3.3 3.3"/>'),
    shield: svg('<path d="M12 3 5 5.6v5.3c0 4.5 3 8.3 7 9.6 4-1.3 7-5.1 7-9.6V5.6z"/><path d="m9 12 2.2 2.2L15.5 10"/>'),
    search: svg('<circle cx="10.5" cy="10.5" r="6.5"/><path d="m15.5 15.5 5 5"/>'),
    vector: svg('<circle cx="6" cy="17" r="2.2"/><circle cx="12" cy="6" r="2.2"/><circle cx="18.5" cy="14" r="2.2"/><path d="M7.5 15.3 10.7 8M14.1 7l2.9 5.3M8.2 17h8.1"/>'),
    site: svg('<path d="M3 8a1.5 1.5 0 0 1 1.5-1.5h4.6l2 2h9.4A1.5 1.5 0 0 1 22 10v8.5A1.5 1.5 0 0 1 20.5 20h-16A1.5 1.5 0 0 1 3 18.5z"/>'),

    // ---- actions
    play: svg('<path d="M8 5.5v13l10.5-6.5z"/>'),
    pause: svg('<path d="M8.5 5.5v13M15.5 5.5v13"/>'),
    stop: svg('<rect x="6.5" y="6.5" width="11" height="11" rx="2"/>'),
    refresh: svg('<path d="M20 12a8 8 0 1 1-2.6-5.9"/><path d="M20 4v4.6h-4.6"/>'),
    open: svg('<path d="M14 4h6v6M20 4l-9 9"/><path d="M19 14v4.5a1.5 1.5 0 0 1-1.5 1.5h-12A1.5 1.5 0 0 1 4 18.5v-12A1.5 1.5 0 0 1 5.5 5H10"/>'),
    folder: svg('<path d="M3 8a1.5 1.5 0 0 1 1.5-1.5h4.6l2 2h9.4A1.5 1.5 0 0 1 22 10v8.5A1.5 1.5 0 0 1 20.5 20h-16A1.5 1.5 0 0 1 3 18.5z"/>'),
    edit: svg('<path d="M4 20h4.2L19 9.2 14.8 5 4 15.8z"/><path d="m13 6.8 4.2 4.2"/>'),
    trash: svg('<path d="M4.5 7h15M9.5 7V4.5h5V7M6.5 7l1 13h9l1-13"/><path d="M10 11v6M14 11v6"/>'),
    x: svg('<path d="m6.5 6.5 11 11M17.5 6.5l-11 11"/>'),
    plus: svg('<path d="M12 5v14M5 12h14"/>'),
    info: svg('<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5"/>' + dot(12, 8, 1.1)),
    file: svg('<path d="M7 3h7l5 5v13H7z"/><path d="M14 3v5h5"/>'),
    terminal: svg('<rect x="3" y="4.5" width="18" height="15" rx="2.5"/><path d="m7 9 3.5 3L7 15M13 15h4"/>'),
    migrate: svg('<path d="M12 3.5v12M8 11.5l4 4 4-4"/><path d="M4 16.5v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2"/>'),
    backup: svg('<path d="M12 3.5v11M8 10.5l4 4 4-4"/><path d="M4 19.5h16"/>'),
    copy: svg('<rect x="8.5" y="8.5" width="12" height="12" rx="2"/><path d="M15.5 8.5v-3a2 2 0 0 0-2-2h-8a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h3"/>'),
    eye: svg('<path d="M2.5 12s3.5-6.5 9.5-6.5 9.5 6.5 9.5 6.5-3.5 6.5-9.5 6.5S2.5 12 2.5 12z"/><circle cx="12" cy="12" r="2.8"/>'),
    spinner: svg('<path d="M20.5 12A8.5 8.5 0 1 1 12 3.5"/>', "spin"),
    sun: svg('<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2.4M12 19.1v2.4M2.5 12h2.4M19.1 12h2.4M5.3 5.3l1.7 1.7M17 17l1.7 1.7M5.3 18.7 7 17M17 7l1.7-1.7"/>'),
    moon: svg('<path d="M20 14.2A8 8 0 0 1 9.8 4a8 8 0 1 0 10.2 10.2z"/>'),
    check: svg('<path d="m5 12.5 4.5 4.5L19 7.5"/>'),

    // ---- remote (SSH client)
    host: svg('<rect x="3.5" y="4" width="17" height="6.5" rx="2"/><rect x="3.5" y="13.5" width="17" height="6.5" rx="2"/>' + dot(7, 7.25, 1.1) + dot(7, 16.75, 1.1) + '<path d="M11 7.25h6M11 16.75h6"/>'),
    hosts: svg('<rect x="3.5" y="4" width="17" height="6.5" rx="2"/><rect x="3.5" y="13.5" width="17" height="6.5" rx="2"/>' + dot(7, 7.25, 1.1) + dot(7, 16.75, 1.1) + '<path d="M11 7.25h6M11 16.75h6"/>'),
    key: svg('<circle cx="8" cy="14" r="4.5"/><path d="m11.3 10.7 8.2-8.2M15.5 6.5l2.5 2.5M18 4l2 2"/>'),
    keychain: svg('<circle cx="8" cy="14" r="4.5"/><path d="m11.3 10.7 8.2-8.2M15.5 6.5l2.5 2.5M18 4l2 2"/>'),
    identity: svg('<circle cx="12" cy="8.5" r="3.8"/><path d="M4.5 20c0-3.6 3.4-6 7.5-6s7.5 2.4 7.5 6"/>'),
    sftp: svg('<path d="M3 8a1.5 1.5 0 0 1 1.5-1.5h4.6l1.8 2H19.5A1.5 1.5 0 0 1 21 10v8.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/><path d="M12 11.5v6M9.5 14 12 11.5l2.5 2.5"/>'),
    forward: svg('<path d="M3 8h13M13 4.5 16.5 8 13 11.5"/><path d="M21 16H8M11 12.5 7.5 16l3.5 3.5"/>'),
    snippet: svg('<path d="m8 7-4.5 5L8 17M16 7l4.5 5L16 17M13.5 5 10.5 19"/>'),
    snippets: svg('<path d="m8 7-4.5 5L8 17M16 7l4.5 5L16 17M13.5 5 10.5 19"/>'),
    history: svg('<path d="M3.8 12a8.2 8.2 0 1 0 2.4-5.8"/><path d="M3.5 4.5v4.5H8M12 8v4.5l3 2"/>'),
    knownhosts: svg('<path d="M12 3 5 5.6v5.3c0 4.5 3 8.3 7 9.6 4-1.3 7-5.1 7-9.6V5.6z"/><path d="m9 12.2 2.2 2.2L15.5 10"/>'),
    group: svg('<path d="M3 8a1.5 1.5 0 0 1 1.5-1.5h4.6l1.8 2H19.5A1.5 1.5 0 0 1 21 10v8.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>'),
    connect: svg('<path d="M9 7V3.5M15 7V3.5M6.5 7h11v3.5a5.5 5.5 0 0 1-11 0z"/><path d="M12 16v4.5"/>'),
    local: svg('<rect x="3" y="4.5" width="18" height="12" rx="2"/><path d="M9 20.5h6M12 16.5v4"/>'),
    upload: svg('<path d="M12 16V5M7.5 9.5 12 5l4.5 4.5"/><path d="M4.5 19.5h15"/>'),
    download: svg('<path d="M12 4.5v11M7.5 11 12 15.5l4.5-4.5"/><path d="M4.5 19.5h15"/>'),
    up: svg('<path d="M12 19V6M6.5 11.5 12 6l5.5 5.5"/>'),
    home: svg('<path d="M4 10.4 12 4l8 6.4"/><path d="M6.2 9.6V19h11.6V9.6"/>'),
    link: svg('<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1.2 1.2"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1.2-1.2"/>'),
    gear: svg('<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>'),
    lock: svg('<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8.5 10.5V7.5a3.5 3.5 0 0 1 7 0v3"/>' + dot(12, 15.5, 1.2)),
    unlock: svg('<rect x="5" y="10.5" width="14" height="10" rx="2"/><path d="M8.5 10.5V7.5a3.5 3.5 0 0 1 6.8-1"/>' + dot(12, 15.5, 1.2)),
    send: svg('<path d="M4 12 20 4l-4 16-4.5-6.5z"/><path d="M11.5 13.5 20 4"/>'),
    dots: svg(dot(6, 12, 1.4) + dot(12, 12, 1.4) + dot(18, 12, 1.4)),
    tag: svg('<path d="M3.5 12.5v-8a1 1 0 0 1 1-1h8l8 8-9 9z"/>' + dot(8, 8, 1.2)),
    import: svg('<path d="M12 4v11M7.5 10.5 12 15l4.5-4.5"/><path d="M4 16.5v2A1.5 1.5 0 0 0 5.5 20h13a1.5 1.5 0 0 0 1.5-1.5v-2"/>'),
    split: svg('<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M12 4.5v15"/>'),
    chevron: svg('<path d="m9 6 6 6-6 6"/>'),
    chevronDown: svg('<path d="m6 9 6 6 6-6"/>'),

    // ---- toasts
    toastOk: svg('<circle cx="12" cy="12" r="8.5"/><path d="m8.5 12.3 2.4 2.4 4.8-5"/>'),
    toastBad: svg('<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5v5"/>' + dot(12, 16, 1.1)),
    toastInfo: svg('<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5"/>' + dot(12, 8, 1.1)),
  };
})();
