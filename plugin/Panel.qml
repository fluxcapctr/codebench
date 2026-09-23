import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Codebench in the bar: a terminal glyph with the number of agent tasks
// that need you, and a dropdown listing them. Data comes from
// `codebench status --follow`, one JSON line per change.
Panel {
  id: root
  moduleName: "codebench"
  ipcTarget: "codebench"

  property var status: ({ running: false, needs: 0, working: 0, tasks: [] })

  readonly property bool running: status.running === true
  readonly property int needs: Number(status.needs) || 0
  readonly property int working: Number(status.working) || 0
  readonly property var tasks: status.tasks ? status.tasks : []

  readonly property color fg: bar ? bar.foreground : Color.foreground
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color dim: Qt.darker(fg, 1.5)
  readonly property color urgent: bar ? bar.urgent : Color.urgent

  readonly property string label: {
    if (needs > 0) return " " + needs
    if (working > 0) return " " + working
    return ""
  }
  readonly property color tone: {
    if (!running) return Color.muted
    if (needs > 0) return urgent
    if (working > 0) return Color.accent
    return bar ? bar.barForeground : Color.foreground
  }

  // ------------------------------------------------------------ feed

  Process {
    id: feed
    command: ["sh", "-c", "exec \"${CODEBENCH:-codebench}\" status --follow 2>/dev/null || exec \"$HOME/.local/bin/codebench\" status --follow"]
    running: true
    stdout: SplitParser {
      onRead: function(line) {
        var text = String(line).trim()
        if (text === "") return
        try { root.status = JSON.parse(text) } catch (e) { }
      }
    }
    onExited: retry.restart()
  }

  Timer {
    id: retry
    interval: 3000
    repeat: false
    onTriggered: feed.running = true
  }

  function openTask(id) {
    // The id goes in as an argument, never into the shell text.
    Quickshell.execDetached(["sh", "-c", "exec codebench --task \"$1\" >/dev/null 2>&1", "sh", String(id)])
    root.close()
  }

  function openApp() {
    Quickshell.execDetached(["sh", "-c", "exec codebench >/dev/null 2>&1"])
    root.close()
  }

  // ------------------------------------------------------------ bar button

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.label
    foreground: root.tone
    fontSize: Style.font.bodySmall
    tooltipText: ""
    dimmed: !root.running
    useActiveColor: false
    onPressed: function(b) { root.toggle() }
  }

  Item {
    id: anchorProbe
    anchors.right: button.right
    anchors.top: button.top
    width: 1
    height: button.height
  }

  // ------------------------------------------------------------ dropdown

  KeyboardPanel {
    id: panel
    anchorItem: anchorProbe
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(380))
    contentHeight: panel.fittedContentHeight(body.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Column {
        id: body
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(10)

        Text {
          text: "CODEBENCH"
          color: Color.accent
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          font.letterSpacing: 1.2
        }

        Text {
          width: parent.width
          wrapMode: Text.WordWrap
          visible: root.tasks.length === 0
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          text: root.running ? "Nothing needs you. No agent is working." : "Codebench is closed."
        }

        Column {
          width: parent.width
          spacing: Style.space(2)
          Repeater {
            model: root.tasks
            Item {
              required property var modelData
              width: parent.width
              height: rowText.implicitHeight + Style.space(10)

              Rectangle {
                anchors.fill: parent
                color: rowMouse.containsMouse ? Qt.rgba(root.fg.r, root.fg.g, root.fg.b, 0.08) : "transparent"
              }
              Text {
                id: rowText
                anchors.verticalCenter: parent.verticalCenter
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: Style.space(6)
                elide: Text.ElideRight
                textFormat: Text.StyledText
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
                color: root.fg
                text: "<font color='" + (modelData.kind === "needs" ? root.urgent : Color.accent) + "'>●</font> "
                  + "<font color='" + root.dim + "'>" + modelData.project + " ›</font> "
                  + modelData.title
                  + "  <font color='" + root.dim + "'>" + modelData.status + "</font>"
              }
              MouseArea {
                id: rowMouse
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.PointingHandCursor
                onClicked: root.openTask(modelData.id)
              }
            }
          }
        }

        Button {
          width: parent.width
          text: root.running ? "Open Codebench" : "Start Codebench"
          fontSize: Style.font.bodySmall
          foreground: root.fg
          fontFamily: root.fontFamily
          bordered: true
          onClicked: root.openApp()
        }
      }
    }
  }
}
