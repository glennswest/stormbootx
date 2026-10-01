@echo -off
# stormbootx: start the boot agent from an EFI Shell fallback (#60).
#
# Firmware with no boot option for this media drops to its built-in shell,
# which runs startup.nsh. Find the stormbootx media whatever fsN it is and
# start its BOOTX64.EFI. stormboot.conf is the mark: a local disk's ESP has a
# BOOTX64.EFI too (stormuefi on an installed disk, a stale Windows), never
# this file. Kept to what the EDK shell 2.31 (EFI 1.10) and Shell 2.x both
# take: for/endfor, nested if exist/endif, no and, no goto.
for %i in fs0 fs1 fs2 fs3 fs4 fs5 fs6 fs7
  if exist %i:\stormboot\stormboot.conf then
    if exist %i:\EFI\BOOT\BOOTX64.EFI then
      echo startup.nsh: starting stormbootx from %i:
      %i:
      \EFI\BOOT\BOOTX64.EFI
    endif
  endif
endfor
echo startup.nsh: back at the shell: stormbootx returned, or no media on fs0..fs7
