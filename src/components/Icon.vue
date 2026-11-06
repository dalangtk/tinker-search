<script setup lang="ts">
const props = withDefaults(
  defineProps<{ name: string; size?: number; stroke?: number }>(),
  { size: 16, stroke: 1.7 }
);

const P: Record<string, string> = {
  search: '<circle cx="11" cy="11" r="7"/><path d="m20.5 20.5-3.9-3.9"/>',
  x: '<path d="M18 6 6 18M6 6l12 12"/>',
  minus: '<path d="M5 12h14"/>',
  maximize: '<rect x="4.5" y="4.5" width="15" height="15" rx="2"/>',
  restore: '<rect x="7" y="7" width="12" height="12" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>',
  pin: '<path d="M12 17v5M9.5 10.8a2 2 0 0 1-1 1.7l-1.7.9A2 2 0 0 0 5.8 15v2h12.4v-2a2 2 0 0 0-1-1.6l-1.7-.9a2 2 0 0 1-1-1.7V6h1a2 2 0 0 0 0-4H8.5a2 2 0 0 0 0 4h1z"/>',
  filter: '<path d="M3.5 4.5h17l-6.6 7.7v6.2l-3.8 1.9v-8.1z"/>',
  sliders: '<path d="M4 21v-6M4 11V3M12 21v-9M12 8V3M20 21v-4M20 13V3"/><circle cx="4" cy="13" r="2"/><circle cx="12" cy="10" r="2"/><circle cx="20" cy="15" r="2"/>',
  chevron: '<path d="m6 9 6 6 6-6"/>',
  folder: '<path d="M3 8a2 2 0 0 1 2-2h3.8a2 2 0 0 0 1.6-.8l1.2-1.6A2 2 0 0 1 13.2 3H19a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  copy: '<rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
  trash: '<path d="M3 6h18M8 6V4.5A1.5 1.5 0 0 1 9.5 3h5A1.5 1.5 0 0 1 16 4.5V6M18.5 6l-.8 13.2A2 2 0 0 1 15.7 21H8.3a2 2 0 0 1-2-1.8L5.5 6"/>',
  edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>',
  star: '<path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2L12 17.3 6.4 20.2l1.1-6.2L3 9.6l6.2-.9z"/>',
  grid: '<rect x="3.5" y="3.5" width="7" height="7" rx="1.5"/><rect x="13.5" y="3.5" width="7" height="7" rx="1.5"/><rect x="3.5" y="13.5" width="7" height="7" rx="1.5"/><rect x="13.5" y="13.5" width="7" height="7" rx="1.5"/>',
  list: '<path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/>',
  dense: '<path d="M4 6h16M4 11h16M4 16h16M4 21h16"/>',
  refresh: '<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3.5V8h-4.5"/>',
  database: '<ellipse cx="12" cy="5.5" rx="8" ry="3"/><path d="M4 5.5v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/><path d="M4 11.5v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/>',
  external: '<path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><path d="M15 3h6v6"/><path d="M10.5 13.5 21 3"/>',
  shield: '<path d="M12 21s7-3.4 7-9V5.6L12 3 5 5.6V12c0 5.6 7 9 7 9z"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 8h.01"/>',
  clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3 2"/>',
  tag: '<path d="M20.6 13.4 13 21a2 2 0 0 1-2.8 0l-7-7A2 2 0 0 1 2.6 13V4a2 2 0 0 1 2-2h9a2 2 0 0 1 1.4.6l5.6 5.6a2 2 0 0 1 0 2.8z"/><path d="M7.5 7.5h.01"/>',
  location: '<path d="M20 10c0 6-8 12-8 12s-8-6-8-12a8 8 0 0 1 16 0z"/><circle cx="12" cy="10" r="3"/>',
  doc: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
  up: '<path d="M12 19V5M5 12l7-7 7 7"/>',
  down: '<path d="M12 5v14M19 12l-7 7-7-7"/>',
  check: '<path d="m5 13 4.5 4.5L19 7"/>',
  save: '<path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><path d="M7 3v6h9M7 21v-6h10v6"/>',
  help: '<circle cx="12" cy="12" r="9"/><path d="M9.5 9.5A2.5 2.5 0 1 1 12 12v1.5M12 17h.01"/>',
  minusCircle: '<circle cx="12" cy="12" r="9"/><path d="M8.5 12h7"/>',
  plusCircle: '<circle cx="12" cy="12" r="9"/><path d="M12 8.5v7M8.5 12h7"/>',
  drive: '<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 13h.01M10 13h.01"/><path d="M2.5 12h19"/>',
  eye: '<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>',
  home: '<path d="M3 10.5 12 3l9 7.5"/><path d="M5.5 9.5V20h13V9.5"/>',
  desktop: '<rect x="2.5" y="3.5" width="19" height="13" rx="2"/><path d="M9 20.5h6M12 16.5v4"/>',
  download: '<path d="M12 3.5v11"/><path d="m7.5 10 4.5 4.5 4.5-4.5"/><path d="M4 20h16"/>',
  image: '<rect x="3" y="4.5" width="18" height="15" rx="2"/><circle cx="8.4" cy="9.6" r="1.5"/><path d="m4 17 4.8-4.8 3.6 3.6 3-2.6L20 17.5"/>',
  music: '<path d="M9 18V6.2l10-2V16"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="16.5" cy="16" r="2.5"/>',
  video: '<rect x="2.5" y="6" width="13" height="12" rx="2"/><path d="m15.5 10.5 6-3.2v9.4l-6-3.2z"/>',
  code: '<path d="m8 8-4 4 4 4"/><path d="m16 8 4 4-4 4"/><path d="m13.4 5-2.8 14"/>',
  archive: '<rect x="3.5" y="4" width="17" height="16" rx="2"/><path d="M10 4v4h4V4"/><path d="M10 12.5h4"/><path d="M12 16.5h.01"/>',
  app: '<rect x="3.5" y="3.5" width="7" height="7" rx="1.6"/><rect x="13.5" y="3.5" width="7" height="7" rx="1.6"/><rect x="3.5" y="13.5" width="7" height="7" rx="1.6"/><rect x="13.5" y="13.5" width="7" height="7" rx="1.6"/>',
  layers: '<path d="m12 3 9 5-9 5-9-5z"/><path d="m3.5 12.5 8.5 4.7 8.5-4.7"/><path d="m3.5 16.8 8.5 4.7 8.5-4.7"/>',
  grid2: '<rect x="3.5" y="3.5" width="17" height="7" rx="2"/><rect x="3.5" y="13.5" width="17" height="7" rx="2"/>',
  memory:
    '<rect x="6.5" y="6.5" width="11" height="11" rx="2"/><path d="M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3"/>',
};
</script>

<template>
  <svg
    :width="props.size"
    :height="props.size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    :stroke-width="props.stroke"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
    v-html="P[props.name] || ''"
  />
</template>
