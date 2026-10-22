// quen's status line in the game's HUD (quen-mcp w3\README.md, section HUD line): one line of text on a dark rounded
// box, in the HUD's dormant WatermarkModule slot, which modQuen registers at run time (quen.ws QUEN_HudLoad). Compiled by
// w3\hud.py with Apache Flex's mxmlc against the game's own classes, kept external: this movie holds this class
// only, and the HUD supplies HudModuleBase and the rest, as it loads every module into its own ApplicationDomain
// (Hud.as loadModule). Shown and hidden by HudModuleBase's ShowElement, as every module is; placed by the script half
// (CR4HudModuleWatermark's anchor, mcAnchorWatermark at 660, 0), sized by the HUD-size option (SetScaleFromWS) and
// faded to the UI opacity option (SetMaxOpacity), as vanilla's modules are.
package quen
{
	import flash.display.Sprite;
	import flash.text.TextField;
	import flash.text.TextFieldAutoSize;
	import flash.text.TextFormat;
	import flash.text.TextFormatAlign;
	import red.core.events.GameEvent;
	import red.game.witcher3.hud.modules.HudModuleBase;

	public class QuenHud extends HudModuleBase
	{
		// ponytail: placed by eye for 1920 x 1080: the anchor + 300 is the screen's centre, and the box's top sits below
		// the boss and oxygen bars (hud_anchors: 960, 23 and 960, 49.9). The probe boot's screenshot tunes them.
		private static const CENTRE : Number = 300;
		private static const TOP : Number = 96;
		private static const WIDTH : Number = 1000;  // the longest line before it wraps
		private static const PAD_X : Number = 16;
		private static const PAD_Y : Number = 6;

		public var tfText : TextField = new TextField();
		private var box : Sprite = new Sprite();

		public function QuenHud()
		{
			super();
			// $NormalFont, embedded, as vanilla's own run-time fields (HudModuleEnemyFocus.as createDamageTextField):
			// the game's font map gives each language its font.
			var format : TextFormat = new TextFormat("$NormalFont", 22, 0xFFFFFF);
			format.align = TextFormatAlign.CENTER;
			tfText.name = "tfText";
			tfText.embedFonts = true;
			tfText.defaultTextFormat = format;
			tfText.selectable = false;
			tfText.mouseEnabled = false;
			tfText.multiline = true;
			tfText.wordWrap = true;
			tfText.width = WIDTH;
			tfText.autoSize = TextFieldAutoSize.CENTER;
			tfText.x = CENTRE - WIDTH / 2;
			tfText.y = TOP + PAD_Y;
			addChild(box);
			addChild(tfText);
		}

		override public function get moduleName() : String
		{
			return "WatermarkModule";
		}

		// As vanilla's HudModuleWatermark: the module comes up hidden; OnConfigUI runs the script half's
		// (CR4HudModuleWatermark: its anchor, opacity and scale).
		override protected function configUI() : void
		{
			super.configUI();
			visible = true;
			alpha = 0;
			dispatchEvent(new GameEvent(GameEvent.CALL, 'OnConfigUI'));
		}

		// The line, as plain text, and the box around it.
		public function SetText(value : String) : void
		{
			tfText.text = value;
			var w : Number = tfText.textWidth + 2 * PAD_X + 4;
			var h : Number = tfText.textHeight + 2 * PAD_Y + 4;
			box.graphics.clear();
			box.graphics.beginFill(0x000000, 0.6);
			box.graphics.drawRoundRect(CENTRE - w / 2, TOP, w, h, 12, 12);
			box.graphics.endFill();
		}
	}
}
