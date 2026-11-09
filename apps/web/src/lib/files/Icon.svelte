<script lang="ts">
  let { name, size = 18, filled = false }: {
    name: string;
    size?: number;
    filled?: boolean;
  } = $props();

  const paths: Readonly<Record<string, string>> = {
    folder: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
    'folder-plus': 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM12 10v7m-3.5-3.5h7',
    file: 'M6 3h8l4 4v14H6zM14 3v5h4',
    text: 'M6 3h8l4 4v14H6zM14 3v5h4M9 12h6m-6 4h6',
    pdf: 'M6 3h8l4 4v14H6zM14 3v5h4M9 16l2-5 2 5m-3.2-1.5h2.4M15 16v-5',
    image: 'M3 5h18v14H3zM3 16l5-5 4 4 3-3 6 6M15.5 9h.01',
    video: 'M4 6h16v12H4zM10 9l5 3-5 3z',
    audio: 'M9 17V6l11-2v11M9 17a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0zm11-2a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z',
    archive: 'M4 4h16v4H4zM5 8v12h14V8M10 12h4',
    star: 'm12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2-5.6-3-5.6 3 1.1-6.2L3 9.6l6.2-.9z',
    clock: 'M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0zm-9-5v5l3 2',
    trash: 'M3 6h18M9 6V3h6v3m4 0-1 15H6L5 6m5 4v7m4-7v7',
    search: 'M16 10a6 6 0 1 1-12 0 6 6 0 0 1 12 0zm-.8 4.2L21 20',
    list: 'M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01',
    grid: 'M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z',
    upload: 'M12 16V3m-5 5 5-5 5 5M4 15v5h16v-5',
    download: 'M12 3v13m-5-5 5 5 5-5M4 16v4h16v-4',
    plus: 'M12 5v14M5 12h14',
    check: 'm5 12 4 4L19 6',
    close: 'm6 6 12 12M18 6 6 18',
    refresh: 'M20 8a8 8 0 1 0 0 8M20 4v4h-4',
    back: 'm10 6-6 6 6 6M4 12h16',
    forward: 'm14 6 6 6-6 6M20 12H4',
    up: 'm6 10 6-6 6 6M12 4v16',
    down: 'm6 14 6 6 6-6M12 20V4',
    'chevron-right': 'm9 5 7 7-7 7',
    'chevron-left': 'm15 5-7 7 7 7',
    'chevron-down': 'm5 9 7 7 7-7',
    'arrow-left': 'm10 6-6 6 6 6M4 12h16',
    'arrow-right': 'm14 6 6 6-6 6M20 12H4',
    'arrow-up': 'm6 10 6-6 6 6M12 4v16',
    'arrow-down': 'm6 14 6 6 6-6M12 20V4',
    'arrow-up-right': 'M7 17 17 7M7 7h10v10',
    menu: 'M4 6h16M4 12h16M4 18h16',
    more: 'M5 12h.01M12 12h.01M19 12h.01',
    edit: 'm4 20 4-1L20 7l-3-3L5 16zM14 7l3 3',
    rename: 'm4 20 4-1L20 7l-3-3L5 16zM14 7l3 3',
    move: 'M12 3v18M3 12h18m-9-9-2 2m2-2 2 2m-2 16-2-2m2 2 2-2M3 12l2-2m-2 2 2 2m16-2-2-2m2 2-2 2',
    copy: 'M8 8h12v12H8zM16 8V4H4v12h4',
    scissors: 'M6 5a3 3 0 1 0 0 6 3 3 0 0 0 0-6zm0 8a3 3 0 1 0 0 6 3 3 0 0 0 0-6zm2.5-3L20 19M8.5 14 20 5',
    clipboard: 'M8 5H5v16h14V5h-3M8 3h8v4H8zM8 11h8m-8 4h8',
    restore: 'M4 4v6h6M4 10a8 8 0 1 1 0 5',
    key: 'M14 9a5 5 0 1 1 0 .01zm1.5 3.5L5 23m4-4-2-2m5-1-2-2',
    lock: 'M5 10h14v11H5zM8 10V7a4 4 0 0 1 8 0v3m-4 4v3',
    shield: 'M12 3 4 6v5c0 5.2 3.2 8.3 8 10 4.8-1.7 8-4.8 8-10V6zm-3 9 2 2 4-4',
    drive: 'M5 5h14l2 11v4H3v-4zm-2 11h18M7 18h.01m3 0h.01',
    info: 'M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0zm-9-4h.01m0 3v5',
    settings: 'M4 7h16M4 17h16M8 4v6m8 4v6',
    filter: 'M4 5h16l-6 7v6l-4 2v-8z',
    sort: 'M4 5h16M4 12h11M4 19h6',
    logout: 'M9 4H4v16h5m6-4 4-4-4-4m-6 4h10',
    eye: 'M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6-10-6-10-6zm13 0a3 3 0 1 1-6 0 3 3 0 0 1 6 0z'
  };
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  viewBox="0 0 24 24"
  width={size}
  height={size}
  fill={filled ? 'currentColor' : 'none'}
  stroke="currentColor"
  stroke-width="1.65"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
>
  <path d={paths[name] ?? paths.file} />
</svg>
