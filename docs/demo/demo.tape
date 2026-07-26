# Regenerate with: vhs docs/demo/demo.tape (run from the repo root, diffctx on PATH)
Output docs/demo/demo.gif

Set Shell bash
Set FontSize 15
Set Width 1000
Set Height 700
Set Padding 16
Set TypingSpeed 35ms

Hide
Type "source scripts/demo-setup.sh && clear"
Enter
Sleep 2s
Show

Type "git log --oneline -2"
Enter
Sleep 2s

Type "diffctx . --diff HEAD~1"
Enter
Sleep 5s

Type "diffctx . --diff HEAD~1 -c"
Enter
Sleep 4s
