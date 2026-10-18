import { defineConfig, presetUno } from 'unocss';

export default defineConfig({
  presets: [presetUno()],
  shortcuts: {
    'vm-panel': 'bg-white dark:bg-[#1b1b1f] rounded-lg border border-solid border-[#e4e7ed] dark:border-[#2b2b31]',
    'vm-row': 'flex items-center gap-2',
    'vm-muted': 'text-[#909399] text-xs',
  },
});
