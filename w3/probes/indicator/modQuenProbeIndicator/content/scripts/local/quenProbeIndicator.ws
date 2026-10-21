// modQuenProbeIndicator: a PROBE for quen's in-game status indicator, not for play (README.md beside this mod).
// Can the game's own GUI carry a lasting status line, and hide it at once? Console execs show the overlay notification
// for up to an hour and hide it, add a run-time table entry for a dormant HUD module (AreaInfo, or Watermark) and create
// it, show, hide and read back both. Every line goes to LogChannel('QuenInd'). Nothing runs until typed. No resources, no
// wrapped vanilla method; one added field and two added methods on the player (the walker probe's pattern).

// One watch at a time: after a hide, the tick looks for each hidden element's sprite to read hidden.
class QuenIndWatch
{
	var t0, blink : float;  // engine time of the hide; qiblink: engine time its hide is due, 0 none
	var note, mod : bool;   // still watched: the notification, the module
	var ticks, which : int; // timer calls since the hide; the module slot
}

@addField(CR4Player) var quenInd : QuenIndWatch;

@addMethod(CR4Player) function QuenIndGet() : QuenIndWatch
{
	if (!quenInd)
		quenInd = new QuenIndWatch in this;
	return quenInd;
}

// Every tick while a hide is watched or a blink waits: qiblink's hide when it is due, then the first tick on which each
// watched element reads hidden (its sprite gone, not visible, or alpha 0), with the engine time since the hide. Gives up
// after 3 s. The world's clock: it stops in photo mode and under the pause menu.
@addMethod(CR4Player) timer function QuenIndTick(dt : float, id : int)
{
	var s : CScriptedFlashSprite;
	var now, t : float;

	now = theGame.GetEngineTimeAsSeconds();
	if (quenInd.blink > 0 && now >= quenInd.blink)
	{
		quenInd.blink = 0;
		QuenIndHideBoth(quenInd.which, "blink");
	}
	if (quenInd.note || quenInd.mod)
	{
		quenInd.ticks += 1;
		t = now - quenInd.t0;
		if (quenInd.note)
		{
			s = QuenIndNoteSprite();
			if (!s || !s.GetVisible() || s.GetAlpha() <= 0)
			{
				quenInd.note = false;
				LogChannel('QuenInd', "watch notification hidden t=" + FloatToStringPrec(t, 3) + " ticks=" + quenInd.ticks + " " + QuenIndSprite(s));
			}
		}
		if (quenInd.mod)
		{
			s = QuenIndModuleSprite(quenInd.which);
			if (!s || !s.GetVisible() || s.GetAlpha() <= 0)
			{
				quenInd.mod = false;
				LogChannel('QuenInd', "watch module hidden t=" + FloatToStringPrec(t, 3) + " ticks=" + quenInd.ticks + " " + QuenIndSprite(s));
			}
		}
		if ((quenInd.note || quenInd.mod) && t > 3)
		{
			LogChannel('QuenInd', "watch gave up after 3 s: still shown notification=" + quenInd.note + " module=" + quenInd.mod);
			quenInd.note = false;
			quenInd.mod = false;
		}
	}
	if (!quenInd.note && !quenInd.mod && quenInd.blink == 0)
		RemoveTimer('QuenIndTick');
}

function QuenIndWatchFrom(note : bool, mod : bool, which : int)
{
	var w : QuenIndWatch;

	if (!thePlayer)
	{
		LogChannel('QuenInd', "watch skipped: no player");
		return;
	}
	w = thePlayer.QuenIndGet();
	w.t0 = theGame.GetEngineTimeAsSeconds();
	w.ticks = 0;
	w.note = note;
	w.mod = mod;
	w.which = which;
	thePlayer.AddTimer('QuenIndTick', 0.001, true, , , , true);
}

// The module slots: 0 AreaInfoModule, 1 WatermarkModule, with the file names and depths of the two entries Hud.as's
// table has commented out (gui_new actionscript red\game\witcher3\hud\Hud.as 222-254).
function QuenIndSlot(which : int, out modName : string, out file : string, out depth : int) : bool
{
	if (which == 0)
	{
		modName = "AreaInfoModule";
		file = "hud_areainfo.swf";
		depth = 14;
		return true;
	}
	if (which == 1)
	{
		modName = "WatermarkModule";
		file = "hud_watermark.swf";
		depth = 10;
		return true;
	}
	LogChannel('QuenInd', "refused: module slot " + which + " is not 0 (AreaInfo) or 1 (Watermark)");
	return false;
}

function QuenIndPopup() : CR4OverlayPopup
{
	var gui : CR4GuiManager = theGame.GetGuiManager();

	if (!gui)
		return NULL;
	return (CR4OverlayPopup)gui.GetPopup('OverlayPopup');
}

// The notification's sprite: OverlayPopupMenu.as's public notificationModule (a NotificationModule).
function QuenIndNoteSprite() : CScriptedFlashSprite
{
	var popup : CR4OverlayPopup = QuenIndPopup();
	var flash : CScriptedFlashSprite;

	if (!popup)
		return NULL;
	flash = popup.GetPopupFlash();
	if (!flash)
		return NULL;
	return flash.GetChildFlashSprite("notificationModule");
}

// The module's sprite: NULL until its movie has loaded and registered.
function QuenIndModuleSprite(which : int) : CScriptedFlashSprite
{
	var hud : CHud = theGame.GetHud();
	var module : CHudModule;
	var modName, file : string;
	var depth : int;

	if (!hud || !QuenIndSlot(which, modName, file, depth))
		return NULL;
	module = hud.GetHudModule(modName);
	if (!module)
		return NULL;
	return module.GetModuleFlash();
}

function QuenIndSprite(s : CScriptedFlashSprite) : string
{
	if (!s)
		return "sprite=null";
	return "visible=" + s.GetVisible() + " alpha=" + FloatToStringPrec(s.GetAlpha(), 2) + " x=" + FloatToStringPrec(s.GetX(), 1)
		+ " y=" + FloatToStringPrec(s.GetY(), 1) + " scale=" + FloatToStringPrec(s.GetXScale(), 2);
}

function QuenIndText(s : CScriptedFlashSprite, field : string) : string
{
	var tf : CScriptedFlashTextField;

	if (!s)
		return "text=null";
	tf = s.GetChildFlashTextField(field);
	if (!tf)
		return "text=null";
	return "text=[" + tf.GetText() + "]";
}

// A HUD module's Flash state (HudModuleBase.as stateMachine.current): Hide, Show, OnDemand or OnUpdate. Also a test of
// GetMemberFlashObject on a plain AS3 object.
function QuenIndFsm(s : CScriptedFlashSprite) : string
{
	var sm : CScriptedFlashObject;

	if (!s)
		return "null";
	sm = s.GetMemberFlashObject("stateMachine");
	if (!sm)
		return "unreadable";
	return sm.GetMemberFlashString("current");
}

// The run-time entry (design 3.2): AddEntry(name, file, depth) on the HUD root's public moduleManager (Hud.as 54), as
// Hud.as's constructor adds the other 29. Every read first: a null changes nothing. True when the table holds the name
// afterwards, or when AddEntry ran and the table can't be read back. The fallback's wrap calls it too.
function QuenIndEntry(hud : CHud, which : int) : bool
{
	var root : CScriptedFlashSprite;
	var mm, found : CScriptedFlashObject;
	var entries : CScriptedFlashArray;
	var add : CScriptedFlashFunction;
	var modName, file : string;
	var depth, n : int;

	if (!QuenIndSlot(which, modName, file, depth))
		return false;
	if (!hud)
	{
		LogChannel('QuenInd', "entry refused: no HUD");
		return false;
	}
	root = hud.GetHudFlash();
	if (!root)
	{
		LogChannel('QuenInd', "entry refused: GetHudFlash() gave null");
		return false;
	}
	LogChannel('QuenInd', "entry root " + QuenIndSprite(root) + " isDynamic=" + root.GetMemberFlashBool("isDynamic")
		+ " loadError=" + root.GetMemberFlashInt("loadError"));
	mm = root.GetMemberFlashObject("moduleManager");
	if (!mm)
	{
		LogChannel('QuenInd', "entry refused: GetMemberFlashObject(moduleManager) gave null");
		return false;
	}
	add = mm.GetMemberFlashFunction("AddEntry");
	if (!add)
	{
		LogChannel('QuenInd', "entry refused: moduleManager.GetMemberFlashFunction(AddEntry) gave null");
		return false;
	}
	n = -1;
	entries = mm.GetMemberFlashArray("entries");
	if (entries)
	{
		n = entries.GetLength();
		if (GetObjectFromArrayWithLabel(entries, "m_name", modName, found))
		{
			LogChannel('QuenInd', "entry " + modName + " already in the table (" + n + " entries): not added again");
			return true;
		}
	}
	add.InvokeSelfThreeArgs(FlashArgString(modName), FlashArgString(file), FlashArgInt(depth));
	entries = mm.GetMemberFlashArray("entries");
	if (!entries)
	{
		LogChannel('QuenInd', "entry " + modName + " " + file + " depth=" + depth + ": AddEntry called; the table (a Vector) reads as no array, so unconfirmed");
		return true;
	}
	if (GetObjectFromArrayWithLabel(entries, "m_name", modName, found))
	{
		LogChannel('QuenInd', "entry " + modName + " " + file + " depth=" + depth + " added: entries " + n + " -> " + entries.GetLength());
		return true;
	}
	LogChannel('QuenInd', "entry refused: AddEntry called but the table (" + n + " -> " + entries.GetLength() + " entries) doesn't hold " + modName);
	return false;
}

// The lasting notification as phase 1 would send it: the GUI manager's ShowNotification with a duration (quen.ws's
// notice passes 8000), queue false, so it replaces one on screen after that one's 0.3 s hide (OverlayPopupMenu.as
// showNotification). NotificationModule.as runs a Flash Timer of the duration, then hides.
function QuenIndNoteShow(text : string, secs : float) : bool
{
	var gui : CR4GuiManager = theGame.GetGuiManager();
	var had : bool;

	if (!gui)
	{
		LogChannel('QuenInd', "note refused: no GUI manager");
		return false;
	}
	if (QuenIndPopup())
		had = true;  // else ShowNotification requests the popup, with the text as its init data
	if (text == "")
		text = "quen: agent at work. Please keep off the mouse and keys.";
	if (secs <= 0)
		secs = 3600;
	gui.ShowNotification(text, secs * 1000);
	LogChannel('QuenInd', "note shown duration_ms=" + FloatToStringPrec(secs * 1000, 0) + " popup_before=" + had
		+ " now " + QuenIndSprite(QuenIndNoteSprite()) + " " + QuenIndText(QuenIndNoteSprite(), "tfMessage"));
	return true;
}

// The notification's hide: HideNotification (Flash hideNotification: a 0.3 s tween, then visible false), or with clear
// ClearNotificationsQueue (the same tween, and a queued notification dropped). No vanilla script calls the first.
function QuenIndNoteHide(clear : bool) : bool
{
	var popup : CR4OverlayPopup = QuenIndPopup();

	if (!popup || !popup.GetPopupFlash())
	{
		LogChannel('QuenInd', "note hide refused: no overlay popup");
		return false;
	}
	if (clear)
		popup.ClearNotificationsQueue();
	else
		popup.HideNotification();
	LogChannel('QuenInd', "note hide sent clear=" + clear + " now " + QuenIndSprite(QuenIndNoteSprite()));
	return true;
}

// The module's text and show. A module loaded after the HUD's last input context sits in its Flash Hide state (its
// table entry had no state to give it, Hud.as 337), where ShowElement only records the wish: then UpdateHUD() sends the
// current context again (hud.ws 1544) first. force: ShowElement's third argument, bIgnoreState (HudModuleBase.as 119).
function QuenIndModShow(text : string, which : int, force : bool) : bool
{
	var hud : CR4ScriptedHud;
	var s : CScriptedFlashSprite;
	var show, setText : CScriptedFlashFunction;
	var fsm : string;

	hud = (CR4ScriptedHud)theGame.GetHud();
	s = QuenIndModuleSprite(which);
	if (!hud || !s)
	{
		LogChannel('QuenInd', "show refused: module slot " + which + " not loaded (qiadd, then qistate)");
		return false;
	}
	show = s.GetMemberFlashFunction("ShowElement");
	setText = s.GetMemberFlashFunction("SetText");
	if (!show || (which == 0 && !setText))
	{
		LogChannel('QuenInd', "show refused: ShowElement or SetText gave null");
		return false;
	}
	fsm = QuenIndFsm(s);
	if (fsm != "OnDemand" && !force)
	{
		hud.UpdateHUD();
		LogChannel('QuenInd', "show: state " + fsm + ", UpdateHUD() for context " + hud.currentInputContext + ": state now " + QuenIndFsm(s));
	}
	if (which == 0)
	{
		if (text == "")
			text = "quen: agent at work";
		setText.InvokeSelfOneArg(FlashArgString(text));
	}
	if (force)
		show.InvokeSelfThreeArgs(FlashArgBool(true), FlashArgBool(true), FlashArgBool(true));
	else
		show.InvokeSelfTwoArgs(FlashArgBool(true), FlashArgBool(true));
	LogChannel('QuenInd', "show slot=" + which + " force=" + force + " now " + QuenIndSprite(s) + " state=" + QuenIndFsm(s) + " " + QuenIndText(s, "textField"));
	return true;
}

// The module's hide: ShowElement(false, true) as hudModuleBase.ws sends it (in OnDemand it also records the wish), then
// ShowElement(false, true, true), which hides in any state. Both set visible and alpha within the call.
function QuenIndModHide(which : int) : bool
{
	var s : CScriptedFlashSprite = QuenIndModuleSprite(which);
	var show : CScriptedFlashFunction;

	if (!s)
	{
		LogChannel('QuenInd', "hide refused: module slot " + which + " not loaded");
		return false;
	}
	show = s.GetMemberFlashFunction("ShowElement");
	if (!show)
	{
		LogChannel('QuenInd', "hide refused: ShowElement gave null");
		return false;
	}
	show.InvokeSelfTwoArgs(FlashArgBool(false), FlashArgBool(true));
	show.InvokeSelfThreeArgs(FlashArgBool(false), FlashArgBool(true), FlashArgBool(true));
	LogChannel('QuenInd', "hide slot=" + which + " in the same call: " + QuenIndSprite(s) + " state=" + QuenIndFsm(s));
	return true;
}

function QuenIndHideBoth(which : int, why : string)
{
	var note, mod : bool;

	note = QuenIndNoteHide(true);
	mod = QuenIndModuleSprite(which) && QuenIndModHide(which);
	LogChannel('QuenInd', why + " hid notification=" + note + " module=" + mod + " t=" + FloatToStringPrec(theGame.GetEngineTimeAsSeconds(), 3));
	QuenIndWatchFrom(note, mod, which);
}

// qinote(text, secs): the notification, up an hour by default (secs 3600) or until qinhide. Text default: phase 1's
// "working" line. At most 100 bytes through game_exec.
exec function qinote(optional text : string, optional secs : float)
{
	QuenIndNoteShow(text, secs);
}

// qinhide(clear): the notification's hide; clear true also drops a queued one. The tick logs when it reads hidden.
exec function qinhide(optional clear : bool)
{
	if (QuenIndNoteHide(clear))
		QuenIndWatchFrom(true, false, 0);
}

// qiadd(which): the dormant module of slot which (0 AreaInfo, default; 1 Watermark) made at run time, in this order:
// every read, the table entry (QuenIndEntry), DiscardHudModule (vanilla OnConfigUI's CreateHudModule found no entry
// and loaded nothing), CreateHudModule. Its movie loads on a later tick: qistate reads it. Does nothing once loaded.
exec function qiadd(optional which : int)
{
	var hud : CHud = theGame.GetHud();
	var module : CHudModule;
	var modName, file, had : string;
	var depth : int;

	if (!QuenIndSlot(which, modName, file, depth))
		return;
	if (!hud)
	{
		LogChannel('QuenInd', "add refused: no HUD");
		return;
	}
	module = hud.GetHudModule(modName);
	if (module && module.GetModuleFlash())
	{
		LogChannel('QuenInd', "add " + modName + ": already loaded " + QuenIndSprite(module.GetModuleFlash()));
		return;
	}
	had = "false";
	if (module)
		had = "true";
	LogChannel('QuenInd', "add " + modName + ": before, script object=" + had);
	if (!QuenIndEntry(hud, which))
	{
		LogChannel('QuenInd', "add " + modName + ": no entry, nothing discarded or created");
		return;
	}
	hud.DiscardHudModule(modName);
	had = "false";
	if (hud.GetHudModule(modName))
		had = "true";
	LogChannel('QuenInd', "add " + modName + ": DiscardHudModule called, script object=" + had);
	hud.CreateHudModule(modName);
	module = hud.GetHudModule(modName);
	if (module)
		LogChannel('QuenInd', "add " + modName + ": CreateHudModule called, script object=true " + QuenIndSprite(module.GetModuleFlash()) + " (the movie loads on a later tick: qistate)");
	else
		LogChannel('QuenInd', "add " + modName + ": CreateHudModule called, script object=false (the movie loads on a later tick: qistate)");
}

// qishow(text, which, force): the module's text (AreaInfo only: Watermark shows its own) and ShowElement(true, true).
exec function qishow(optional text : string, optional which : int, optional force : bool)
{
	QuenIndModShow(text, which, force);
}

// qihide(which): the module's hide, read back in the same call; the tick logs it too.
exec function qihide(optional which : int)
{
	if (QuenIndModHide(which))
		QuenIndWatchFrom(false, true, which);
}

// qiblink(after, which): both shown with their default texts, both hidden after s later (default 2.5) by the tick, the
// notification with ClearNotificationsQueue. For measure_ab: settle 3 then starts the window about 0.5 s after the hide.
exec function qiblink(optional after : float, optional which : int)
{
	var w : QuenIndWatch;
	var mod : bool;

	if (!thePlayer)
	{
		LogChannel('QuenInd', "blink refused: no player");
		return;
	}
	if (after <= 0)
		after = 2.5;
	QuenIndNoteShow("", 0);
	mod = QuenIndModuleSprite(which) && QuenIndModShow("", which, false);
	w = thePlayer.QuenIndGet();
	w.blink = theGame.GetEngineTimeAsSeconds() + after;
	w.which = which;
	thePlayer.AddTimer('QuenIndTick', 0.001, true, , , , true);
	LogChannel('QuenInd', "blink shown module=" + mod + " hide due in " + FloatToStringPrec(after, 2) + " s at t=" + FloatToStringPrec(w.blink, 3));
}

// qisave(on): the overlay's saving spinner on or off (CR4GuiManager.ShowSavingIndicator / HideSavingIndicator(true)):
// does it last? A text-free "busy" marker.
exec function qisave(optional on : bool)
{
	var gui : CR4GuiManager = theGame.GetGuiManager();
	var popup : CR4OverlayPopup = QuenIndPopup();

	if (!gui || !popup || !popup.GetPopupFlash())
	{
		LogChannel('QuenInd', "save refused: no GUI manager or overlay popup");
		return;
	}
	if (on)
		gui.ShowSavingIndicator();
	else
		gui.HideSavingIndicator(true);
	LogChannel('QuenInd', "save indicator on=" + on + " now " + QuenIndSprite(popup.GetPopupFlash().GetChildFlashSprite("mcIndicatorSave")));
}

// qistate(which): read-backs only, changes nothing. The notification (sprite, text), the spinner, the HUD root, the input
// context, the module of slot which (script object, sprite, Flash state, enabled, text) and the engine time.
exec function qistate(optional which : int)
{
	var hud : CR4ScriptedHud;
	var popup : CR4OverlayPopup = QuenIndPopup();
	var module : CHudModule;
	var s, root : CScriptedFlashSprite;
	var modName, file : string;
	var depth : int;

	hud = (CR4ScriptedHud)theGame.GetHud();
	s = QuenIndNoteSprite();
	LogChannel('QuenInd', "state t=" + FloatToStringPrec(theGame.GetEngineTimeAsSeconds(), 3) + " note "
		+ QuenIndSprite(s) + " " + QuenIndText(s, "tfMessage"));
	if (popup && popup.GetPopupFlash())
		LogChannel('QuenInd', "state spinner " + QuenIndSprite(popup.GetPopupFlash().GetChildFlashSprite("mcIndicatorSave")));
	if (!hud || !QuenIndSlot(which, modName, file, depth))
	{
		LogChannel('QuenInd', "state hud: none");
		return;
	}
	root = hud.GetHudFlash();
	if (root)  // loadError: Hud.as counts the module movies that failed to load (handleMovieLoadError)
		LogChannel('QuenInd', "state hud root " + QuenIndSprite(root) + " loadError=" + root.GetMemberFlashInt("loadError") + " context=" + hud.currentInputContext);
	else
		LogChannel('QuenInd', "state hud root: GetHudFlash() gave null");
	module = hud.GetHudModule(modName);
	if (!module)
	{
		LogChannel('QuenInd', "state " + modName + ": script object=false");
		return;
	}
	s = module.GetModuleFlash();
	LogChannel('QuenInd', "state " + modName + ": script object=true " + QuenIndSprite(s) + " state=" + QuenIndFsm(s)
		+ " enabled=" + (s && s.GetMemberFlashBool("isEnabled")) + " " + QuenIndText(s, "textField"));
}
