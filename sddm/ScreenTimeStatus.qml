/*
 * Screen-time banner for the SDDM login screen and the Plasma lock screen.
 * login-theme.nix copies Plasma's Breeze SDDM theme, drops this file next to
 * its Main.qml and patches Main.qml to show it and to explain refused logins;
 * lock-screen.nix does the same for the lock screen's LockScreenUi.qml.
 *
 * Runs `screentime status --json USER` every few seconds through Plasma's
 * "executable" data engine (plasma5support). If that fails (daemon not up
 * yet, engine missing, user not managed) the banner just stays hidden;
 * logging in and unlocking are unaffected.
 */
import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.kde.plasma.components as PlasmaComponents3
import org.kde.plasma.plasma5support as P5Support

Rectangle {
    id: banner

    // Set by login-theme.nix / lock-screen.nix. `user` is only needed to
    // explain refused logins; the name shown comes from the status.
    property string command
    property string user

    // The parsed status, or null while it's unavailable.
    property var status: null

    readonly property string name: status === null ? ""
        : status.user.charAt(0).toUpperCase() + status.user.slice(1)
    readonly property bool limited: status !== null && status.allowed_now && !status.unlimited
    readonly property color levelColor: {
        if (!limited) {
            return Kirigami.Theme.textColor;
        }
        if (status.remaining_secs <= 5 * 60) {
            return Kirigami.Theme.negativeTextColor;
        }
        return status.remaining_secs <= 15 * 60 ? Kirigami.Theme.neutralTextColor
                                                : Kirigami.Theme.positiveTextColor;
    }

    visible: status !== null
    implicitWidth: content.implicitWidth + Kirigami.Units.gridUnit * 3
    implicitHeight: content.implicitHeight + Kirigami.Units.gridUnit * 1.5
    radius: Kirigami.Units.gridUnit
    color: Qt.rgba(0, 0, 0, 0.45)

    // 5400 -> "1h 30m", like the CLI.
    function duration(secs) {
        const h = Math.floor(secs / 3600);
        const m = Math.floor((secs % 3600) / 60);
        if (h === 0 && m === 0) {
            return `${secs}s`;
        }
        if (h === 0) {
            return `${m}m`;
        }
        return m === 0 ? `${h}h` : `${h}h ${m}m`;
    }

    function nextText() {
        return status.next_window ? `Next: ${status.next_window}`
                                  : "No allowed time in the coming week";
    }

    // Why screentime refused `loginUser`'s login, or "" if it didn't (wrong
    // password, another user, status unknown). Short, since it shares the
    // password box's width; the banner below says when it's next allowed.
    function refusal(loginUser) {
        if (status === null || user === "" || loginUser !== status.user || status.allowed_now) {
            return "";
        }
        return status.cutoff === "window" ? "Outside allowed hours"
                                          : "No screen time left today";
    }

    P5Support.DataSource {
        engine: "executable"
        connectedSources: [banner.command]
        interval: 10000
        onNewData: (source, data) => {
            try {
                banner.status = data["exit code"] === 0 ? JSON.parse(data["stdout"]) : null;
            } catch (e) {
                banner.status = null;
            }
        }
    }

    ColumnLayout {
        id: content
        anchors.centerIn: parent
        spacing: Kirigami.Units.largeSpacing

        PlasmaComponents3.Label {
            Layout.alignment: Qt.AlignHCenter
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 2
            font.weight: Font.DemiBold
            text: {
                if (banner.status === null) {
                    return "";
                }
                const who = banner.name;
                if (banner.status.unlimited) {
                    return `${who} · no limit today`;
                }
                if (banner.status.allowed_now) {
                    return `${who} · ${banner.duration(banner.status.remaining_secs)} left`;
                }
                return banner.status.cutoff === "window" ? `${who} · outside allowed hours`
                                                         : `${who} · no screen time left today`;
            }
        }

        // Used of today's limit, coloured like the tray icon.
        Rectangle {
            visible: banner.limited
            Layout.alignment: Qt.AlignHCenter
            implicitWidth: Kirigami.Units.gridUnit * 20
            implicitHeight: Kirigami.Units.smallSpacing * 2
            radius: height / 2
            color: Qt.rgba(1, 1, 1, 0.2)

            Rectangle {
                width: banner.limited && banner.status.limit_secs > 0
                    ? parent.width * Math.min(1, banner.status.used_secs / banner.status.limit_secs)
                    : 0
                height: parent.height
                radius: parent.radius
                color: banner.levelColor
            }
        }

        PlasmaComponents3.Label {
            Layout.alignment: Qt.AlignHCenter
            opacity: 0.85
            font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.2
            visible: text !== ""
            text: {
                const st = banner.status;
                if (st === null || st.unlimited) {
                    return "";
                }
                if (!st.allowed_now) {
                    return banner.nextText();
                }
                const used = `${banner.duration(st.used_secs)} of ${banner.duration(st.limit_secs)} used`;
                return st.window_end ? `${used} · allowed until ${st.window_end}` : used;
            }
        }
    }
}
