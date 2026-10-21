// modQuenProbeIndicatorWrap: the indicator probe's FALLBACK (README.md beside it), staged beside modQuenProbeIndicator
// only when its run-time entry (qiadd) failed. It needs that mod's QuenIndEntry.
// The one wrapped vanilla method, because it is the one script hook between the HUD movie's construction and its module
// loop: the HUD's own OnConfigUI (hud.ws 340-412) creates AreaInfoModule (385), so with the table entry added first the
// vanilla CreateHudModule finds it and loads the movie, and no Discard or late Create is needed.
@wrapMethod(CR4ScriptedHud) function OnConfigUI()
{
	QuenIndEntry(this, 0);
	wrappedMethod();
}
