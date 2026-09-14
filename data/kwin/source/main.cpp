// SPDX-License-Identifier: GPL-3.0-or-later

#include <input.h>
#include <plugin.h>

#include <QDBusAbstractAdaptor>
#include <QDBusConnection>
#include <QDBusError>
#include <QDebug>

namespace
{
constexpr auto service = "io.github.staphylococcus.LGBuddy.KWinInhibition";
constexpr auto path = "/io/github/staphylococcus/LGBuddy/KWinInhibition";

class InhibitionAdaptor : public QDBusAbstractAdaptor
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "io.github.staphylococcus.LGBuddy.KWinInhibition1")

public:
    explicit InhibitionAdaptor(QObject *parent)
        : QDBusAbstractAdaptor(parent)
    {
    }

public Q_SLOTS:
    bool IsInhibited() const
    {
        return !KWin::input()->idleInhibitors().isEmpty();
    }

    QString BuildId() const
    {
        return QStringLiteral(LG_BUDDY_KWIN_BUILD_ID);
    }

    QString BuildVersion() const
    {
        return QStringLiteral(KWIN_PLUGIN_VERSION_STRING);
    }
};

class InhibitionBridge : public KWin::Plugin
{
public:
    InhibitionBridge()
    {
        new InhibitionAdaptor(this);
    }

    bool registerOnBus()
    {
        auto bus = QDBusConnection::sessionBus();
        objectRegistered = bus.registerObject(QString::fromLatin1(path), this);
        if (objectRegistered) {
            serviceRegistered = bus.registerService(QString::fromLatin1(service));
        }
        if (!serviceRegistered) {
            qWarning() << "LG Buddy KWin bridge registration failed:" << bus.lastError();
        }
        return serviceRegistered;
    }

    ~InhibitionBridge() override
    {
        auto bus = QDBusConnection::sessionBus();
        if (serviceRegistered) {
            bus.unregisterService(QString::fromLatin1(service));
        }
        if (objectRegistered) {
            bus.unregisterObject(QString::fromLatin1(path));
        }
    }

private:
    bool objectRegistered = false;
    bool serviceRegistered = false;
};
}

class KWIN_EXPORT InhibitionFactory : public KWin::PluginFactory
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID PluginFactory_iid FILE "metadata.json")
    Q_INTERFACES(KWin::PluginFactory)

public:
    std::unique_ptr<KWin::Plugin> create() const override
    {
        if (!KWin::input()) {
            return {};
        }
        auto bridge = std::make_unique<InhibitionBridge>();
        if (!bridge->registerOnBus()) {
            return {};
        }
        return bridge;
    }
};

#include "main.moc"
