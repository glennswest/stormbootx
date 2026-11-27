@echo -off
# stormbootx: start the boot agent from an EFI Shell fallback (#60).
#
# Firmware with no boot option for this media drops to its built-in shell,
# which runs startup.nsh. Find the stormbootx media whatever fsN it is and
# start its BOOTX64.EFI. stormboot.conf is the mark: a local disk's ESP has a
# BOOTX64.EFI too (stormuefi on an installed disk, a stale Windows), never
# this file. Kept to what the EDK shell 2.31 (EFI 1.10) and Shell 2.x both
# take: for/endfor, nested if exist/endif, no and, no goto.
#
# One medium only (#180): a stormbootx that returned has already said where the
# machine boots, and a second medium's is older or the same (server7: its stick,
# then a BMC's v0.10.0 CD). An unset variable or a shell with no set -v reads as
# not started, so the worst this can do is the old behaviour.
#
# The guard is deleted first (#263): server8's AMI shell (X9, Aptio 4) kept
# `set -v stormbootx_started yes` across a reset, and every boot after the
# first then skipped every medium. Deleting it here makes each run of this
# script start from nothing, whatever the shell stored. A shell that never had
# it may say it is not found; that is harmless. Then it is set to `no`,
# volatile: whatever the store kept, the loop below reads `no`.
#
# Every step says what it found (#263, P0): server8's shell went from the
# countdown straight to `back at the shell` twice more with the guard
# deleted, and `@echo -off` hid which test failed. The guard as found, each
# mapped fsN and what it carries, and what stormbootx returned are printed.
echo startup.nsh: stormbootx's script; the guard was "%stormbootx_started%"
set -d stormbootx_started
set -v stormbootx_started no
# And as the stored kind too: a shell that keeps the guard non-volatile and
# takes neither the delete nor a volatile set over it still reads `no` from
# here. The line after says which won.
set stormbootx_started no
echo startup.nsh: the guard is now "%stormbootx_started%" (it must say no)
for %i in fs0 fs1 fs2 fs3 fs4 fs5 fs6 fs7
  if exist %i:\ then
    if not exist %i:\stormboot\stormboot.conf then
      echo startup.nsh: %i: no \stormboot\stormboot.conf: not stormbootx media
    endif
    if exist %i:\stormboot\stormboot.conf then
      if not exist %i:\EFI\BOOT\BOOTX64.EFI then
        echo startup.nsh: %i: \stormboot\stormboot.conf but no \EFI\BOOT\BOOTX64.EFI
      endif
      if "%stormbootx_started%" == "yes" then
        echo startup.nsh: %i: stormbootx media, not started: one medium only
      endif
    endif
  endif
  if not "%stormbootx_started%" == "yes" then
    if exist %i:\stormboot\stormboot.conf then
      if exist %i:\EFI\BOOT\BOOTX64.EFI then
        echo startup.nsh: starting stormbootx from %i:
        set -v stormbootx_started yes
        %i:
        \EFI\BOOT\BOOTX64.EFI
        echo startup.nsh: stormbootx on %i: returned to the shell
      endif
    endif
  endif
endfor
echo startup.nsh: back at the shell: stormbootx returned, or no media on fs0..fs7
