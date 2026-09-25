# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.
import json
import glob
import os
import re
import sys
import time
import uuid
import secrets
import http.client
import logging
import cherrypy
import threading

from lxml import etree
from queue import Queue
from enum import Enum
from cherrypy import _cpnative_server

from .utils import load_xml, XMLPath, Setting, SettingProperty, cherrypy_publish, SETTING_DIR, LOG_FILE_NAME
from .discovery import advertisable_addresses
from . import plugin_repo
from . import logsplit
from . import mirror_view
from . import module_settings

logger = logging.getLogger("Protocol")
logger.setLevel(logging.INFO)

SERVICE_STATE_OBSERVED = {
    "AVTransport": ['TransportState',
                    'TransportStatus',
                    'CurrentMediaDuration',
                    'CurrentTrackDuration',
                    'CurrentTrack',
                    'NumberOfTracks'],
    "RenderingControl": ['Volume', 'Mute'],
    "ConnectionManager": ['A_ARG_TYPE_Direction',
                          'SinkProtocolInfo',
                          'CurrentConnectionIDs']
}

SOAP_ENV_NS = 'http://schemas.xmlsoap.org/soap/envelope/'
SOAP_ENCODING_NS = 'http://schemas.xmlsoap.org/soap/encoding/'
UPNP_CONTROL_NS = 'urn:schemas-upnp-org:control-1-0'


def normalize_soap_body(rawbody):
    """Drop a leading BOM / indentation that precedes the XML declaration.

    XML requires the declaration to be the very first thing in the document,
    so expat rejects any leading whitespace with "XML declaration allowed only
    at the start of the document". Clients do send it anyway: BiliPai
    (Android, okhttp) pretty-prints the whole envelope and indents even the
    declaration by 12 spaces:

        b'            <?xml version="1.0" encoding="utf-8"?>\\n            <s:Envelope ...>'

    which made every SetAVTransportURI fail with HTTP 500 and the cast look
    like it silently did nothing. Leading whitespace carries no meaning here,
    so strip it (plus a UTF-8 BOM) instead of refusing the request.

    Accepts bytes (the usual path from the HTTP body) or str, and always
    returns bytes: lxml refuses a *str* that carries an encoding declaration,
    so a decoded body must never be handed back to it.
    """
    if isinstance(rawbody, str):
        rawbody = rawbody.encode('utf-8')
    return rawbody.lstrip(b'\xef\xbb\xbf \t\r\n')


def soap_fault(error_code, error_description, fault_code='s:Client'):
    """Serialise a UPnP SOAP Fault (control-1-0) as a response body.

    A DLNA control point can only interpret a SOAP Fault; the HTML page
    CherryPy renders for an unhandled exception is opaque to it, so a bad
    request ends up looking like "casting does nothing".
    """
    root = etree.Element(etree.QName(SOAP_ENV_NS, 'Envelope'),
                         nsmap={'s': SOAP_ENV_NS})
    root.attrib['{{{}}}encodingStyle'.format(SOAP_ENV_NS)] = SOAP_ENCODING_NS
    body = etree.SubElement(root, etree.QName(SOAP_ENV_NS, 'Body'))
    fault = etree.SubElement(body, etree.QName(SOAP_ENV_NS, 'Fault'))
    etree.SubElement(fault, 'faultcode').text = fault_code
    etree.SubElement(fault, 'faultstring').text = 'UPnPError'
    detail = etree.SubElement(fault, 'detail')
    error = etree.SubElement(detail, etree.QName(UPNP_CONTROL_NS, 'UPnPError'))
    etree.SubElement(error, 'errorCode').text = str(error_code)
    etree.SubElement(error, 'errorDescription').text = error_description
    return etree.tostring(root, encoding='UTF-8', xml_declaration=False)


class PlaybackGuard:
    """Transparent renderer wrapper that enforces one playback owner.

    With several protocols live, two senders can cast at the same time. The
    player itself resolves that by switching track, but nobody tells the
    displaced sender — its phone keeps showing a progress bar for something
    that stopped.

    Every protocol reaches the player through ``Protocol.renderer``, so this
    is the one place the handoff can be observed. All attribute access is
    forwarded unchanged; only ``set_media_url`` (start of playback) triggers
    ownership transfer.
    """

    #: owner of the current playback, or None
    owner = None

    def __init__(self, protocol, renderer):
        self._protocol = protocol
        self._renderer = renderer

    def set_media_url(self, *args, **kwargs):
        previous = PlaybackGuard.owner
        if previous is not None and previous is not self._protocol:
            logger.info("Playback taken over from %s by %s",
                        type(previous).__name__, type(self._protocol).__name__)
            try:
                previous.release_playback()
            except Exception as e:  # never block playback on a notification
                logger.error("release_playback failed on %s: %s",
                             type(previous).__name__, e)
        PlaybackGuard.owner = self._protocol
        return self._renderer.set_media_url(*args, **kwargs)

    def __getattr__(self, name):
        # Only called when normal lookup misses, so this never shadows a real
        # renderer attribute.
        return getattr(self._renderer, name)


class Protocol:
    def __init__(self):
        self._handler = None

    @property
    def handler(self):
        if self._handler is None:
            self._handler = Handler()
        return self._handler

    def start(self):
        pass

    def stop(self):
        pass

    def reload(self):
        self.stop()
        self.start()

    def methods(self):
        return list(filter(lambda m: m.startswith('set_state_') and callable(getattr(self, m)), dir(self)))

    @property
    def renderer(self):
        renderers = cherrypy.engine.publish('get_renderer')
        if len(renderers) == 0:
            logger.error("Unable to find an available renderer.")
            return None
        renderer = renderers.pop()
        if renderer is None:
            return None
        return PlaybackGuard(self, renderer)

    def release_playback(self):
        """Another protocol took over playback; release control.

        There is one player behind every protocol. When a second sender starts
        casting, mpv just switches track and the displaced sender's app keeps
        showing controls for a stream it no longer owns. The default does
        nothing; protocols able to signal STOP to their client can override
        this. The point is that the handoff is *offered* at a single choke
        point rather than being each protocol's problem.
        """
        pass

    # The following methods are called by the renderer to set the playback status within the protocol,
    # which will be passed to the client (generally the mobile phone)

    def set_state_position(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        pass

    def set_state_duration(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        pass

    def set_state_pause(self):
        """
        :return:
        """
        pass

    def set_state_play(self):
        """
        :return:
        """
        pass

    def set_state_stop(self):
        """
        :return:
        """
        pass

    def set_state_eof(self):
        """
        :return:
        """
        pass

    def set_state_transport(self, data: str):
        """
        :param data: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        :return:
        """
        pass

    def set_state_transport_error(self):
        """
        :return:
        """
        pass

    def set_state_mute(self, data: bool):
        """
        :param data: bool
        :return:
        """
        pass

    def set_state_volume(self, data: int):
        """
        :param data: int 0-100
        :return:
        """
        pass

    def set_state_speed(self, data: str):
        pass

    def set_state_display_subtitle(self, data: bool):
        """ set custom subtitle file path
        :param data: bool, whether display the subtitle
        :return:
        """
        pass

    def set_state_url(self, data: str):
        pass

    def set_state(self, state_name, state_value):
        pass

    def get_state(self, state_name):
        return ''

    def get_state_title(self) -> str:
        """
        :return: string, eg: demo
        """
        return ''

    def get_state_url(self) -> str:
        """
        :return: string, eg: http://10.10.10.10/demo.mp4
        """
        return ''

    def get_state_position(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return '00:00:00'

    def get_state_duration(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return '00:00:00'

    def get_state_volume(self) -> int:
        """
        :return: int, range from 0 to 100
        """
        return 80

    def get_state_mute(self) -> bool:
        """
        :return: bool
        """
        return False

    def get_state_transport_state(self) -> str:
        """
        :return: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        """
        return 'STOPPED'

    def get_state_transport_status(self) -> str:
        """
        :return: string in [OK, ERROR_OCCURRED]
        """
        return 'OK'

    def get_state_speed(self) -> str:
        return '1'

    def get_state_display_subtitle(self) -> bool:
        return True


class ObserveClient:
    def __init__(self, service, url, timeout=1800):
        self.url = url
        self.service = service
        self.startTime = int(time.time())
        self.sid = "uuid:{}".format(uuid.uuid4())
        self.timeout = timeout
        self.seq = 0
        self.host = re.findall(r"//([0-9:.]*)", url)[0]
        self.path = re.findall(r"//[0-9:.]*(.*)$", url)[0]
        print("-----------------------------", self.host)
        self.error = 0

    def is_timeout(self):
        return int(time.time()) - self.startTime > self.timeout

    def update(self, timeout=1800):
        self.startTime = int(time.time())
        self.timeout = timeout

    def send_event_callback(self, data):
        """Sending event data to client
        """
        headers = {"NT": "upnp:event",
                   "NTS": "upnp:propchange",
                   "CONTENT-TYPE": 'text/xml; charset="utf-8"',
                   "SERVER": Setting.get_server_info(),
                   "SID": self.sid,
                   "SEQ": self.seq,
                   "TIMEOUT": "Second-{}".format(self.timeout)
                   }
        namespace = 'urn:schemas-upnp-org:event-1-0'
        root = etree.Element(etree.QName(namespace, 'propertyset'),
                             nsmap={'e': namespace})
        if self.service == 'ConnectionManager':
            for i in data:
                prop = etree.SubElement(
                    root, '{urn:schemas-upnp-org:event-1-0}property')
                item = etree.SubElement(prop, i)
                item.text = str(data[i])
        else:
            prop = etree.SubElement(
                root, '{urn:schemas-upnp-org:event-1-0}property')
            last_change = etree.SubElement(prop, 'LastChange')
            event = etree.Element('Event')
            event.attrib['xmlns'] = 'urn:schemas-upnp-org:metadata-1-0/AVT/'
            instance_id = etree.SubElement(event, 'InstanceID')
            instance_id.set('val', '0')
            for i in data:
                p = etree.SubElement(instance_id, i)
                p.set('val', str(data[i]))
            last_change.text = etree.tostring(event, encoding="UTF-8").decode()
        data = etree.tostring(root, encoding="UTF-8")
        logger.debug("Prop Change---------")
        logger.debug(data)
        conn = http.client.HTTPConnection(self.host, timeout=5)
        conn.request("NOTIFY", self.path, data, headers)
        conn.close()
        self.seq = self.seq + 1


class DataType(Enum):
    boolean = 'boolean'
    i2 = 'i2'
    ui2 = 'ui2'
    i4 = 'i4'
    ui4 = 'ui4'
    string = 'string'


class StateVariable:
    """The state of render
    """

    def __init__(self, name, send_events, datatype, service):
        self.name = name
        self.sendEvents = True if send_events == 'yes' else False
        self.datatype = DataType(datatype)
        self.minimum = None
        self.maximum = None
        self.allowedValueList = None
        self.value = '' if self.datatype == DataType.string else 0
        self.service = service

    def set_value(self, value):
        self.value = value

    def set_allowed_value_list(self, values):
        self.allowedValueList = values
        if 'NOT_IMPLEMENTED' in values:
            self.value = 'NOT_IMPLEMENTED'

    def set_allowed_value_range(self, minimum, maximum):
        self.minimum = minimum
        self.maximum = maximum


class Argument:
    def __init__(self, name, state, value=None):
        self.name = name
        self.state = state
        self.value = value


class Action:
    """Operations supported by render.

    Parameters
    ----------
    name : string
        The name of the operation.
    input : list<Argument>
        A set of state values.
    output : list<Argument>
        A set of state values.

    """

    def __init__(self, name, input, output):
        self.name = name
        self.input = input
        self.output = output


class Service:
    service_map = {}

    @classmethod
    def get(cls, name):
        return cls.service_map.get(name, Service(name))

    @classmethod
    def build(cls, name, ns, actions):
        cls.service_map[name] = Service(name, ns, actions)

    def __init__(self, name, namespace='', actions={}):
        self.name = name
        self.namespace = namespace
        self.actions = actions


class DLNAProtocol(Protocol):

    def __init__(self):
        super(DLNAProtocol, self).__init__()
        self.running = False
        self.state_list = {}
        self.action_list = {}
        self.event_thread = None
        self.event_subscribes = {}  # subscribe devices
        self.state_queue = Queue()  # states needed be send to subscribe devices
        self.removed_device_queue = Queue()  # devices needed be removed
        self.append_device_queue = Queue()  # devices needed be added
        self._state_event = threading.Event()  # wakes the event thread immediately
        self._init_sem = threading.Semaphore(20)  # bound init-event threads
        self.playlist = []          # cast playlist (list of uris)
        self.current_index = -1     # current playlist position
        self.init_services()  # create services handle function from xml file
        self.init_state()  # set default value

    @property
    def handler(self):
        if self._handler is None:
            self._handler = DLNAHandler()
        return self._handler

    def init_state(self):
        self.set_state('CurrentPlayMode', 'NORMAL')
        self.set_state('TransportPlaySpeed', 1)
        self.set_state('TransportStatus', 'OK')
        self.set_state('RelativeCounterPosition', 2147483647)
        self.set_state('AbsoluteCounterPosition', 2147483647)
        self.set_state('A_ARG_TYPE_Direction', 'Output')
        self.set_state('CurrentConnectionIDs', '0')
        self.set_state('PlaybackStorageMedium', 'None')
        self.set_state('SinkProtocolInfo', load_xml(XMLPath.PROTOCOL_INFO.value).strip())

    def init_services(self, description=XMLPath.DESCRIPTION.value):
        """
        :param description: dlna description xml file
        :return:
        """
        desc = etree.parse(description).getroot()
        for service_type in desc.iter('{urn:schemas-upnp-org:device-1-0}serviceType'):
            # service_type:  urn:schemas-upnp-org:service:ConnectionManager:1
            namespace = service_type.text
            service = service_type.text.split(":")[3]
            self.build_action(namespace, service, etree.parse(
                XMLPath.BASE_PATH.value + f"/xml/{service}.xml").getroot())

    def build_action(self, namespace, service, xml):
        """
        :param namespace: eg: urn:schemas-upnp-org:service:ConnectionManager:1
        :param service:  eg: ConnectionManager
        :param xml: xml content of service
        :return:
        """
        """Build action and variable list from xml file
        """
        ns = '{urn:schemas-upnp-org:service-1-0}'
        # get state variable from xml file
        for state_variable in xml.iter(ns + 'stateVariable'):
            name = state_variable.find(ns + "name").text

            data = StateVariable(name,
                                 state_variable.attrib['sendEvents'],
                                 state_variable.find(ns + "dataType").text,
                                 service)
            default_value = state_variable.find(ns + "defaultValue")
            if default_value is not None:
                data.set_value(default_value.text)
            allowed_value_list = state_variable.find(ns + "allowedValueList")
            if allowed_value_list is not None:
                values = [
                    value.text
                    for value in allowed_value_list.findall(ns + "allowedValue")
                ]
                data.set_allowed_value_list(values)

            allowed_value_range = state_variable.find(ns + "allowedValueRange")
            if allowed_value_range is not None:
                data.set_allowed_value_range(
                    int(allowed_value_range.find(ns + "minimum").text),
                    int(allowed_value_range.find(ns + "maximum").text))
            self.state_list[name] = data

        # get action from xml file
        actions = {}
        for action in xml.iter(ns + 'action'):
            name = action.find(ns + "name").text
            input = []
            output = []
            argument_list = action.find(ns + "argumentList")
            if argument_list is not None:
                for argument in argument_list.findall(ns + 'argument'):
                    data = Argument(
                        argument.find(ns + "name").text,
                        argument.find(ns + "relatedStateVariable").text)
                    if argument.find(ns + "direction").text == 'in':
                        input.append(data)
                    else:
                        output.append(data)
            actions[name] = Action(name, input, output)
        # self.action_list[service] = actions
        Service.build(service, namespace, actions)

    def add_subscribe(self, service, url, timeout=1800):
        """Add a DLNA client to subscribe list
        """
        logger.error("SUBSCRIBE: " + url)
        for client in self.event_subscribes:
            if self.event_subscribes[client].url == url and \
                    self.event_subscribes[client].service == service:
                s = self.event_subscribes[client]
                s.update(timeout)
                logger.error("SUBSCRIBE UPDATE")
                return {
                    "SID": s.sid,
                    "TIMEOUT": "Second-{}".format(s.timeout)
                }
        logger.error("SUBSCRIBE ADD")
        client = ObserveClient(service, url, timeout)
        self.append_device_queue.put(client)
        threading.Thread(target=self._send_init_event,
                         args=(service, client),
                         daemon=True).start()
        return {
            "SID": client.sid,
            "TIMEOUT": "Second-{}".format(client.timeout)
        }

    def _send_init_event(self, service, client):
        """When there is a client subscription,
        the first event callback will send all the state values of the service.
        Bounded by a semaphore so a flood of SUBSCRIBEs cannot spawn
        unbounded threads."""
        with self._init_sem:
            self.send_init_event(service, client)

    def send_init_event(self, service, client):
        """When there is a client subscription,
        the first event callback will send all the state values of the service.
        """
        data = {}
        for state in SERVICE_STATE_OBSERVED[service]:
            data[state] = self.state_list[state].value
        try:
            client.send_event_callback(data)
        except Exception as e:
            logger.error(str(e))

    def remove_subscribe(self, sid):
        """Remove a DLNA client from subscribe list
        """
        if sid in self.event_subscribes:
            self.removed_device_queue.put(sid)
        return 200

    def renew_subscribe(self, sid, timeout=1800):
        """Renew a DLNA client in subcribe list
        """
        if sid in self.event_subscribes:
            self.event_subscribes[sid].update(timeout)
            return 200
        return 412

    def _sync_subscribe_list(self):
        """Apply pending subscriber additions/removals to `event_subscribes`.

        Split out of `send_states_to_clients` because it must not depend on a
        state change having happened. It used to be reachable *only* from
        there, and that call was gated on `state_queue` being non-empty -- so
        with nothing observed changing (playback position is deliberately not
        observed; DLNA clients poll GetPositionInfo for it) a new subscriber
        sat in `append_device_queue` forever. The settings page's client table
        reads `event_subscribes`, so it showed "no clients" for the first
        subscriber of every session, no matter how long it stayed subscribed.

        Returns True if anything changed.
        """
        changed = False
        while not self.removed_device_queue.empty():
            sid = self.removed_device_queue.get()
            logger.info("Remove client: {}".format(sid))
            self.event_subscribes.pop(sid, None)
            self.removed_device_queue.task_done()
            changed = True
        while not self.append_device_queue.empty():
            client = self.append_device_queue.get()
            # `add_subscribe` runs on a CherryPy worker thread; registering
            # here (the event thread) is what keeps the dict single-writer.
            self.event_subscribes[client.sid] = client
            self.append_device_queue.task_done()
            logger.info("Add client: {} ({})".format(client.sid, client.url))
            changed = True
        return changed

    def _reap_timed_out_clients(self):
        """Queue every subscriber whose TIMEOUT has elapsed for removal."""
        for sid in list(self.event_subscribes):
            if self.event_subscribes[sid].is_timeout():
                self.remove_subscribe(sid)

    def send_states_to_clients(self, state_change_list):
        """Sending the states in the stateChangeList to the clients which subscribe to them.
        :param state_change_list:
        :return:
        """
        if not bool(state_change_list):
            return
        self._sync_subscribe_list()
        # send stateChangeList to client
        for sid in list(self.event_subscribes):
            client = self.event_subscribes[sid]
            try:
                if client.is_timeout():
                    self.remove_subscribe(client.sid)
                    continue
                # Only send state which within the service
                state = {}
                for name in state_change_list:
                    if self.state_list[name].service == client.service:
                        state[name] = state_change_list[name]
                if len(state) == 0:
                    continue
                client.send_event_callback(state)

            except Exception as e:
                logger.error("send event error: " + str(e))
                client.error = client.error + 1
                if client.error > 10:
                    logger.debug("remove " + client.sid)
                    self.remove_subscribe(client.sid)

        # A removal queued by the loop above must not wait for the next state
        # change to take effect.
        self._sync_subscribe_list()

    def event(self):
        """DLNA Event thread
        If a DLNA client subscribes to the dlna event,
        it will automatically send the event to the client when the renderer state changes.
        Uses a threading.Event instead of a 1s sleep-poll so state changes
        (volume / position / transport) reach the client with sub-second latency.
        """
        while self.running:
            self._state_event.wait(timeout=0.25)
            self._state_event.clear()
            # Register/reap subscribers on *every* tick, not only when a state
            # has changed. This used to happen only inside
            # send_states_to_clients, which the loop reached only when
            # `state_queue` was non-empty -- so a client that subscribed while
            # nothing observed was changing (playback position is deliberately
            # not observed; DLNA clients poll GetPositionInfo for it) stayed in
            # `append_device_queue` indefinitely, and the settings page's
            # client table showed "no clients" for the first subscriber of
            # every session.
            self._reap_timed_out_clients()
            self._sync_subscribe_list()
            if not self.state_queue.empty():
                state = {}
                while not self.state_queue.empty():
                    k, v = self.state_queue.get()
                    state[k] = v
                    self.state_queue.task_done()
                self.send_states_to_clients(state)

    def call(self, rawbody):
        """Processing requests from DLNA clients
        The request from the client is passed into this method
        through the DLNAHandler(macast.py -> class DLNAHandler).
        If the Render class implements the corresponding action method,
        the method will be called automatically.
        Otherwise, the corresponding state variable will be returned
        according to the **action return value** described in the XML file.
        :param rawbody: soap request from dlna client
        :return:
        """
        try:
            envelope = etree.fromstring(normalize_soap_body(rawbody))
        except etree.XMLSyntaxError as e:
            # Log the head of the offending body -- a client-side XML quirk is
            # otherwise invisible (macast.log is wiped on every app start) --
            # and answer with a SOAP Fault instead of an opaque CherryPy 500
            # HTML page, which no DLNA control point can parse.
            logger.error('SOAP parse failed: {} ({} bytes, head={!r})'.format(
                e, len(rawbody), rawbody[:300]))
            cherrypy.response.status = 500
            return soap_fault(402, 'Invalid Args')
        soap_ns = 'http://schemas.xmlsoap.org/soap/envelope/'
        body = envelope.find('{{{}}}Body'.format(soap_ns))
        if body is None or len(body) == 0:
            raise ValueError('malformed SOAP request: missing Body/action')
        # Skip any comment/processing-instruction nodes before the action.
        action_el = None
        for child in body:
            if isinstance(child.tag, str):
                action_el = child
                break
        if action_el is None:
            raise ValueError('malformed SOAP request: no action element')
        # Use the action element's namespace + local name instead of brittle
        # string splitting on the tag, which breaks on comments/whitespace
        # text nodes or non-standard SOAP namespaces.
        qn = etree.QName(action_el.tag)
        action = qn.localname
        service = qn.namespace.split(':')[-2]
        param = {}
        for node in action_el:
            param[etree.QName(node.tag).localname] = node.text
        method = "{}_{}".format(service, action)
        if method not in [
            'AVTransport_GetPositionInfo',
            'AVTransport_GetTransportInfo',
            'RenderingControl_GetVolume'
        ]:
            logger.info("{} {}".format(method, param))
        res = {}
        service_type = Service.get(service)
        if hasattr(self, method):
            data = {}
            # input = self.action_list[service][action].input
            input = service_type.actions[action].input
            for arg in input:
                data[arg.name] = Argument(
                    arg.name, arg.state,
                    param[arg.name] if arg.name in param else None)
                if arg.name in param:
                    self.set_state(arg.state, param[arg.name])
            res = getattr(self, method)(data)
        else:
            # output = self.action_list[service][action].output
            output = service_type.actions[action].output
            for arg in output:
                res[arg.name] = self.state_list[arg.state].value
        if method not in ['ConnectionManager_GetProtocolInfo', 'AVTransport_GetPositionInfo']:
            logger.info("{}res: {}".format("*" * 20, res))
        else:
            logger.info("{}res: {}".format("*" * 20, method))

        # build response xml
        ns = 'http://schemas.xmlsoap.org/soap/envelope/'
        encoding = 'http://schemas.xmlsoap.org/soap/encoding/'
        root = etree.Element(etree.QName(ns, 'Envelope'), nsmap={'s': ns})
        root.attrib[f'{{{ns}}}encodingStyle'] = encoding
        body = etree.SubElement(root, etree.QName(ns, 'Body'), nsmap={'s': ns})
        # namespace = 'urn:schemas-upnp-org:service:{}:1'.format(service)
        response = etree.SubElement(body,
                                    etree.QName(
                                        service_type.namespace, '{}Response'.format(action)),
                                    nsmap={'u': service_type.namespace})
        for key in res:
            prop = etree.SubElement(response, key)
            prop.text = str(res[key])
        return etree.tostring(root, encoding="UTF-8", xml_declaration=False)

    def set_state(self, name: str, value) -> None:
        """Set DLNA state which defined by xml file
        :param name: state name
        :param value: state value
        :return:
        """
        # update states which will send to DLNA Client
        if name in SERVICE_STATE_OBSERVED['AVTransport'] or \
                name in SERVICE_STATE_OBSERVED['RenderingControl']:
            logger.debug("setState: {} {}".format(name, value))
            # When some states change, the DLNA client needs to be notified immediately
            # We put this kind of state into state_queue, waiting to be sent to client.
            self.state_queue.put((name, value))
            self._state_event.set()
        # update other states
        if self.state_list[name].value != value:
            self.state_list[name].value = value

    def get_state(self, name: str):
        """Get DLNA state, The type of state is described by XML file
        :param name: DLNA state name
        :return: int/string/bool
        """
        return self.state_list[name].value

    def start(self):
        """Start render thread
        """
        if self.running:
            return

        self.running = True
        self.event_thread = threading.Thread(target=self.event, daemon=True)
        self.event_thread.start()
        self.set_state_stop()

    def stop(self):
        """Stop render thread
        """
        self.running = False

    # The following method names are defined by the XML file

    def RenderingControl_SetVolume(self, data):
        volume = data['DesiredVolume']
        self.renderer.set_media_volume(volume.value)
        return {}

    def RenderingControl_SetMute(self, data):
        mute = data['DesiredMute']
        if mute.value == 0 or mute.value == '0':
            mute = False
        else:
            mute = True
        self.renderer.set_media_mute(mute)
        return {}

    def AVTransport_SetAVTransportURI(self, data):
        uri = data['CurrentURI'].value
        logger.info(uri)
        self.set_state_url(uri)
        self.renderer.set_media_url(uri)
        title = Setting.get_friendly_name()
        try:
            # Same tolerance as the SOAP body: a client that indents its
            # envelope indents the embedded DIDL-Lite metadata too, and a
            # leading newline before <?xml...?> would cost us the real title.
            meta = etree.fromstring(
                normalize_soap_body(data['CurrentURIMetaData'].value))
            title_xml = meta.find('.//{{{}}}title'.format(meta.nsmap['dc']))
            if title_xml is not None and title_xml.text is not None:
                title = title_xml.text
            metadata = etree.tostring(meta, encoding="UTF-8", xml_declaration=False)
        except Exception as e:
            logger.error(str(e))
            logger.error(data['CurrentURIMetaData'].value)
            self.set_state('CurrentTrackMetaData', data['CurrentURIMetaData'].value)
        else:
            self.set_state('CurrentTrackMetaData', metadata.decode())
        self.renderer.set_media_title(title)
        self.renderer.set_media_resume()
        self.set_state('CurrentTrackTitle', title)
        self.set_state('CurrentTrackURI', uri)
        # Maintain a simple cast playlist so AVTransport Next/Previous work.
        if uri not in self.playlist:
            self.playlist.append(uri)
        self.current_index = self.playlist.index(uri)
        self.set_state('NumberOfTracks', len(self.playlist))
        self.set_state('CurrentTrack', self.current_index + 1)
        self.set_state('RelativeTimePosition', '00:00:00')
        self.set_state('AbsoluteTimePosition', '00:00:00')
        self.set_state('TransportState', 'PAUSED_PLAYBACK')
        self.set_state('TransportStatus', 'OK')
        self._add_history(uri, title)
        return {}

    def AVTransport_Next(self, data):
        return self._playlist_step(1)

    def AVTransport_Previous(self, data):
        return self._playlist_step(-1)

    def _playlist_step(self, delta):
        if not self.playlist:
            return {}
        idx = self.current_index + delta
        if idx < 0 or idx >= len(self.playlist):
            return {}
        self.current_index = idx
        uri = self.playlist[idx]
        self.set_state_url(uri)
        self.renderer.set_media_url(uri)
        self.set_state('CurrentTrack', idx + 1)
        self.set_state('CurrentTrackURI', uri)
        self.set_state('TransportState', 'PLAYING')
        self.set_state('TransportStatus', 'OK')
        return {}

    def cast_uri(self, uri, title=''):
        """Push a (local or remote) uri to the renderer.

        Used by the local-file-casting flow: the settings page uploads a file,
        Macast serves it from 127.0.0.1, and publishes 'cast_local_file' which
        this method handles."""
        if not uri:
            return
        self.set_state_url(uri)
        self.renderer.set_media_url(uri)
        if title:
            self.renderer.set_media_title(title)
            self.set_state('CurrentTrackTitle', title)
        if uri not in self.playlist:
            self.playlist.append(uri)
        self.current_index = self.playlist.index(uri)
        self.set_state('NumberOfTracks', len(self.playlist))
        self.set_state('CurrentTrack', self.current_index + 1)
        self.set_state('CurrentTrackURI', uri)
        self.set_state('RelativeTimePosition', '00:00:00')
        self.set_state('AbsoluteTimePosition', '00:00:00')
        self.set_state('TransportState', 'PLAYING')
        self.set_state('TransportStatus', 'OK')
        self._add_history(uri, title)

    def _add_history(self, uri, title=''):
        """Persist a played item to the on-disk play history (most recent first).
        Dedupes by uri and caps the list so the settings file stays small."""
        try:
            history = Setting.get(SettingProperty.Play_History, []) or []
            history = [h for h in history
                       if isinstance(h, dict) and h.get('uri') != uri]
            history.insert(0, {
                'uri': uri,
                'title': title or uri,
                'time': int(time.time()),
            })
            history = history[:50]
            Setting.set(SettingProperty.Play_History, history)
        except Exception as e:
            logger.error('add play history error: %s' % e)

    def clear_play_history(self):
        """Remove all persisted play history."""
        try:
            Setting.set(SettingProperty.Play_History, [])
        except Exception as e:
            logger.error('clear play history error: %s' % e)

    def AVTransport_Play(self, data):
        self.renderer.set_media_resume()
        self.set_state('TransportState', 'PLAYING')
        self.set_state('TransportStatus', 'OK')
        return {}

    def AVTransport_Pause(self, data):
        self.renderer.set_media_pause()
        self.set_state('TransportState', 'PAUSED_PLAYBACK')
        return {}

    def AVTransport_Seek(self, data):
        target = data['Target']
        self.renderer.set_media_position(target.value)
        self.set_state('RelativeTimePosition', target.value)
        self.set_state('AbsoluteTimePosition', target.value)
        return {}

    def AVTransport_Stop(self, data):
        self.renderer.set_media_stop()
        self.set_state('TransportState', 'STOPPED')
        return {}

    # The following methods are usually used to update the states of
    # DLNA Renderer according to the status obtained from the player.
    # So, when your player state changes, call the following methods.
    # For example, when you click the pause button of the player,
    # call "self.protocol.set_state_pause()" from renderer
    # Then, the DLNA client (such as your mobile phone) will
    # automatically get this information and update it to the front-end.

    def set_state_position(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        self.set_state('RelativeTimePosition', data)
        self.set_state('AbsoluteTimePosition', data)

    def set_state_duration(self, data: str):
        """
        :param data: string, eg: 00:00:00
        :return:
        """
        self.set_state('CurrentTrackDuration', data)
        self.set_state('CurrentMediaDuration', data)

    def set_state_pause(self):
        self.set_state_transport('PAUSED_PLAYBACK')

    def set_state_play(self):
        self.set_state_transport('PLAYING')

    def set_state_stop(self):
        self.set_state_transport('STOPPED')

    def set_state_eof(self):
        self.set_state_transport('NO_MEDIA_PRESENT')

    def set_state_transport(self, data: str):
        """
        :param data: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        :return:
        """
        self.set_state('TransportState', data)
        self.set_state('TransportStatus', 'OK')

    def set_state_transport_error(self):
        """
        :return:
        """
        self.set_state('TransportState', 'STOPPED')
        self.set_state('TransportStatus', 'ERROR_OCCURRED')

    def set_state_mute(self, data: bool):
        """
        :param data: bool
        :return:
        """
        self.set_state('Mute', data)

    def set_state_volume(self, data: int):
        """
        :param data: int, range from 0 to 100
        :return:
        """
        self.set_state('Volume', data)

    def set_state_speed(self, data: str):
        self.set_state('TransportPlaySpeed', data)

    def set_state_display_subtitle(self, data: bool):
        self.set_state('DisplayCurrentSubtitle', data)

    def set_state_url(self, data: str):
        self.set_state('CurrentTrackURI', data)

    # When you are implementing another protocol similar to DLNA,
    # you can get the status of DLNA renderer by calling the following methods.
    # Using DLNA protocol usually does not need to pay attention to these methods,
    # because the state of renderer will be read automatically when using DLNA protocol.

    def get_state_title(self) -> str:
        """
        :return: string, eg: demo
        """
        return self.get_state('CurrentTrackTitle')

    def get_state_url(self) -> str:
        """
        :return: string, eg: http://10.10.10.10/demo.mp4
        """
        return self.get_state('CurrentTrackURI')

    def get_state_position(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return self.get_state('RelativeTimePosition')

    def get_state_duration(self) -> str:
        """
        :return: string, eg: 00:00:00
        """
        return self.get_state('CurrentMediaDuration')

    def get_state_volume(self) -> int:
        """
        :return: int, range from 0 to 100
        """
        return self.get_state('Volume')

    def get_state_mute(self) -> bool:
        """
        :return: bool
        """
        return self.get_state('Mute')

    def get_state_transport_state(self) -> str:
        """
        :return: string in [PLAYING, PAUSED_PLAYBACK, STOPPED, NO_MEDIA_PRESENT]
        """
        return self.get_state('TransportState')

    def get_state_transport_status(self) -> str:
        """
        :return: string in [OK, ERROR_OCCURRED]
        """
        return self.get_state('TransportStatus')

    def get_state_speed(self) -> str:
        return self.get_state('TransportPlaySpeed')

    def get_state_display_subtitle(self) -> bool:
        return bool(self.get_state('DisplayCurrentSubtitle'))


#: Serialises the first-time generation of the management token.
_api_token_lock = threading.Lock()


def api_token():
    """The management token, stable across restarts.

    It used to be ``secrets.token_hex(16)`` per process and was never shown
    anywhere, which quietly made every management endpoint unusable from
    outside the loopback interface: a phone Shortcut or a script has no way to
    learn a value that only ever existed in this process's memory. It is now
    persisted in the settings file and displayed on the settings page
    (状态 → 网页投屏入口), so configuring a Shortcut once keeps working.
    """
    if Setting.has(SettingProperty.Api_Token):
        token = Setting.get(SettingProperty.Api_Token, '')
        if token:
            return token
    with _api_token_lock:
        # Another worker thread may have generated it while we waited.
        if Setting.has(SettingProperty.Api_Token):
            token = Setting.get(SettingProperty.Api_Token, '')
            if token:
                return token
        token = secrets.token_hex(16)
        Setting.set(SettingProperty.Api_Token, token)
    return token


#: How much of macast.log `?query=log` hands the settings page by default.
#: The file grows ~27 KB/min on a busy instance (every HTTP access line, every
#: SOAP body, every mpv property change), so a run that lasts a day used to
#: produce tens of MB -- and the page shipped *all* of it to the browser and
#: into the DOM on every load. Default to the tail; callers can ask for more.
LOG_TAIL_LINES = 2000
LOG_TAIL_MAX_LINES = 50000
LOG_TAIL_BYTES = 512 * 1024
LOG_TAIL_MAX_BYTES = 4 * 1024 * 1024
#: Ceiling for `?query=log&all=1`; anything bigger is a download, not a page.
LOG_ALL_MAX_BYTES = 20 * 1024 * 1024


def read_log_tail(path, max_lines=LOG_TAIL_LINES, max_bytes=None):
    """Return ``(text, truncated, size)`` for the *end* of a log file.

    Reads backwards in blocks, so showing the last 2000 lines of a 40 MB log
    does not require loading the whole thing. ``truncated`` reports whether
    anything before the returned text was dropped.

    ``max_bytes`` defaults to a budget that scales with the requested line
    count (~256 B/line, i.e. enough for the long SOAP/URL lines this log is
    full of), otherwise asking for 10000 lines would quietly return 5000.

    Decoding uses ``errors='replace'`` on purpose: the block boundary almost
    always lands inside a multi-byte character (a Chinese media title is enough
    to test that), and a strict decode would raise UnicodeDecodeError instead
    of just showing one replacement glyph.
    """
    try:
        max_lines = int(max_lines)
    except (TypeError, ValueError):
        max_lines = LOG_TAIL_LINES
    max_lines = max(1, min(max_lines, LOG_TAIL_MAX_LINES))
    if max_bytes is None:
        max_bytes = min(LOG_TAIL_MAX_BYTES,
                        max(LOG_TAIL_BYTES, max_lines * 256))
    block = 64 * 1024
    with open(path, 'rb') as f:
        f.seek(0, os.SEEK_END)
        size = f.tell()
        data = b''
        pos = size
        while pos > 0 and data.count(b'\n') <= max_lines \
                and len(data) < max_bytes:
            step = min(block, pos)
            pos -= step
            f.seek(pos)
            data = f.read(step) + data
    truncated = pos > 0
    text = data.decode('utf-8', errors='replace')
    if truncated:
        # The window was cut mid-line; drop that partial line rather than
        # showing half a log record at the top.
        cut = text.find('\n')
        text = text[cut + 1:] if cut != -1 else ''
    lines = text.splitlines()
    if len(lines) > max_lines:
        lines = lines[-max_lines:]
        truncated = True
    return '\n'.join(lines), truncated, size


def read_log_all(path, max_bytes=LOG_ALL_MAX_BYTES):
    """Whole-file flavour of `read_log_tail` (bounded by ``max_bytes``)."""
    with open(path, 'rb') as f:
        data = f.read(max_bytes + 1)
    size = os.path.getsize(path)
    truncated = len(data) > max_bytes
    return data[:max_bytes].decode('utf-8', errors='replace'), truncated, size


#: Content type of `GET /api?query=mirror-snapshot`. PNG only, because that is
#: the one raster a browser decodes and the one the plugin grabs: the preview
#: used to offer PPM as well, for a Tk 8.5 console window that no longer exists.
MIRROR_SNAPSHOT_MIME = 'image/png'


def site_of(target):
    """`(host, port)` out of a URL or Host header value; `(None, None)` if unusable.

    One normaliser for both sides of the same-site comparison, because the two
    spellings differ in exactly the ways that would otherwise silently mismatch:
    `Origin` is an absolute URL with no path, `Referer` has a path, and `Host`
    has neither scheme nor path but does carry the port. Lowercased, a trailing
    dot dropped, a default 80/443 dropped so `http://host` and `host` agree, and
    IPv6 without its brackets (`[::1]` and `::1` are the same site).
    """
    from urllib.parse import urlsplit
    if not target:
        return None, None
    target = str(target).strip()
    if '://' not in target:
        # A bare `Host` value: no scheme, so nothing to tell us the port is 80.
        target = 'http://' + target
    try:
        parts = urlsplit(target)
        host = parts.hostname
    except ValueError:
        return None, None
    if not host:
        return None, None
    try:
        port = parts.port
    except ValueError:
        return None, None
    if port in (80, 443, None):
        port = None
    return host.lower().rstrip('.'), port


def _page_host_names():
    """Host names the settings page may legitimately be opened on.

    The comparison is not "does this resolve to us" -- under DNS rebinding the
    attacker's own domain *does* resolve to 127.0.0.1, and that is the whole
    attack. So the list is the names this machine answers on regardless of what
    anyone's resolver says: loopback, the addresses we advertise on the LAN, and
    this host's own names.
    """
    names = {'127.0.0.1', 'localhost', '::1'}
    try:
        names.update(addr for addr in advertisable_addresses() if addr)
    except Exception:  # pragma: no cover - enumeration is best-effort
        pass
    try:
        # The DLNA-side list too: it falls back to the unfiltered interfaces, so
        # a host whose only address looks unusual to us still recognises the
        # page the user actually opened.
        names.update(addr for addr in Setting.get_advertisable_ip() if addr)
    except Exception:  # pragma: no cover - ditto
        pass
    try:
        import socket
        host = (socket.gethostname() or '').lower().rstrip('.')
        if host:
            names.add(host)
            names.add(host.split('.')[0])
            if not host.endswith('.local'):
                names.add(host + '.local')
    except Exception:  # pragma: no cover - ditto
        pass
    return {n for n in names if n}


class SameSiteError(Exception):
    """Raised by `Handler._same_site` with the message the caller should answer."""


@cherrypy.expose
class Handler:

    def __init__(self):
        self.setting_page = load_xml(XMLPath.SETTING_PAGE.value).encode()
        self.local_dir = os.path.join(SETTING_DIR, 'local_files')
        os.makedirs(self.local_dir, exist_ok=True)

    @property
    def protocol(self) -> Protocol:
        protocols = cherrypy.engine.publish('get_protocol')
        if len(protocols) == 0:
            logger.error("Unable to find an available protocol.")
            return Protocol()
        return protocols.pop()

    def _param(self, name):
        """One value out of the request parameters.

        A caller that sends the same key twice -- a token in the query string of
        a URL *and* in its form body -- is the same token twice, not a wrong one,
        but CherryPy hands such a pair back as a list. Comparing that list to a
        string would 403 a caller who is right, and send them off looking for a
        bad token.
        """
        value = cherrypy.request.params.get(name)
        if isinstance(value, (list, tuple)):
            value = value[0] if value else ''
        if isinstance(value, bytes):
            value = value.decode('utf-8', 'replace')
        return value or ''

    def _token_present(self):
        """Whether this request carries the management token.

        The header is what scripts use; the query parameter is what anything
        that can only open a URL (a phone Shortcut, a bookmarklet) can use.
        """
        token = cherrypy.request.headers.get('X-Macast-Token') or self._param('token')
        return bool(token and str(token) == api_token())

    def _management_allowed(self):
        """Management endpoints must only be reachable from an authenticated
        channel. A channel is trusted when it is:
          - the loopback interface (this machine's own settings page), or
          - received over HTTPS (the admin explicitly opened the https
            endpoint), or
          - carrying a valid X-Macast-Token.
        DLNA control/SUBSCRIBE traffic from the LAN stays open, since that is
        the whole point of a renderer."""
        remote = getattr(cherrypy.request, 'remote', None)
        ip = getattr(remote, 'ip', '127.0.0.1')
        if ip in ('127.0.0.1', '::1', '::ffff:127.0.0.1', 'localhost'):
            return True
        # A request that arrived over the HTTPS admin channel is treated as
        # authenticated: the user explicitly opened https://host:port.
        if getattr(cherrypy.request, 'scheme', 'http') == 'https':
            return True
        return self._token_present()

    def _same_site(self):
        """Whether this request could have come from our own settings page.

        "Loopback means the user's own machine" has always been the weak link in
        `_management_allowed`: any page the user visits can post a *simple* form
        (urlencoded or multipart, so CORS never prefilters it and the browser
        asks nothing first) straight at http://127.0.0.1:<port>/api, and the one
        thing that gives a drive-by away is the `Origin` it carries. Requests
        with no `Origin` and no `Referer` are not browsers -- `curl`, a phone
        Shortcut, Home Assistant -- and stay judged by address and token alone.

        The port is deliberately not compared: a page we serve on one port
        posting to another port of the same host is the same site for our
        purposes (there is no cookie here to ride), and pinning it would break
        the port-fallback Macast itself does when 58880 is taken.

        Raises `SameSiteError` so the caller answers with a message that says
        what to do, instead of a 403 that reads like a broken token.
        """
        headers = cherrypy.request.headers
        origin = headers.get('Origin') or headers.get('Referer')
        if not origin:
            return
        host, port = site_of(origin)
        if host is None:
            raise SameSiteError('无法识别的来源站点')
        if host not in _page_host_names():
            logger.warning('blocked a cross-site API post from origin %s', origin)
            raise SameSiteError(
                '禁止跨站操作：这个请求来自「{}」，而设置页只服务本机自己。'
                '如果你是从别的网址打开的页面，请改用 http://127.0.0.1:{}/ 再试。'.format(
                    host, Setting.get_port()))

    def _cast_url(self, url, title=''):
        """Push an absolute URL to the renderer, as a JSON-ready result.

        Shared by the POST ``cast-uri`` field (the settings page re-casting a
        history entry) and the GET ``cast`` query (Shortcuts, scripts, curl),
        so both validate and report the same way.
        """
        url = (url or '').strip()
        if not url:
            return {'code': 1, 'message': 'missing url'}
        if not re.match(r'^[a-zA-Z][a-zA-Z0-9+.\-]*://', url):
            # A bare path would reach the player as a relative URL and fail far
            # away from here, with an error that says nothing about the cause.
            return {'code': 1,
                    'message': 'url must be absolute, e.g. http://host/movie.mp4'}
        try:
            self.protocol.cast_uri(url, title)
        except Exception as e:
            logger.error('cast url error: %s' % e)
            return {'code': 1, 'message': 'cast failed'}
        logger.info('cast url from web endpoint: %s', url)
        return {'code': 0, 'message': 'success', 'url': url, 'title': title}

    # -- screen mirror console ---------------------------------------------
    #
    # Screen Mirror's control surface is the settings page, and the page reaches
    # it only through these three endpoints. Nothing here knows what Screen
    # Mirror *is*: the plugin is asked for a `console_state` / `console_action`
    # pair, and `macast/mirror_view.py` turns the state into a layout -- so the
    # core stays free of plugin imports. See `ScreenMirrorSetting` in
    # macast/plugins/renderer/screen_mirror.py.

    def _mirror_setting(self):
        """The screen-mirror console surface, whichever renderer is playing.

        Resolved through the plugin manager rather than through the live
        renderer: 电脑投屏 drives its own ffmpeg and its own stream, an act with
        nothing to do with whichever player holds the DLNA stream -- making the
        user select the Screen Mirror renderer first meant two clicks in front
        of a control that was never about the player.
        """
        manager = cherrypy_publish('get_plugin_manager', None)
        if manager is None or not hasattr(manager, 'mirror_setting'):
            return None
        try:
            return manager.mirror_setting()
        except Exception as e:
            logger.error('mirror console surface unavailable: %s' % e)
            return None

    def _mirror_unavailable(self):
        """Which problem the page is being told about.

        "The app has not built its plugin manager yet" is a two-second state of
        a starting app; "no plugin owns the console" means someone switched the
        Screen Mirror renderer off, and only the second one is a thing to fix by
        clicking a checkbox.
        """
        if cherrypy_publish('get_plugin_manager', None) is None:
            return {'code': 1, 'message': '应用还在启动，请一秒后再试'}
        return {'code': 1,
                'message': '没有可用的电脑投屏插件：在「插件」页签里启用 Screen Mirror'}

    def _mirror_state(self):
        """The plugin's facts plus the core's view of them, in one response.

        The pair is deliberately read together: `view_for` is a pure function of
        this same dict, so splitting the calls would let a page render a layout
        derived from a state it no longer has.
        """
        setting = self._mirror_setting()
        if setting is None:
            return self._mirror_unavailable()
        if hasattr(setting, 'request_probes'):
            # The page's first read is what asks for the two ffmpeg-spawning
            # probes; `console_state()` itself never spawns, so a cold machine
            # would otherwise be shown「正在探测…」as a permanent answer.
            setting.request_probes()
        if hasattr(setting, 'request_preview'):
            # Same reason, different grab: the preview's `<img>` is only in the
            # DOM once a frame exists, so nothing in the page ever asks for the
            # first one. This is the read that sponsors it.
            setting.request_preview()
        try:
            state = setting.console_state()
        except Exception as e:
            logger.error('mirror state failed: %s' % e)
            return {'code': 1, 'message': 'mirror state failed: {}'.format(e)}
        return {'code': 0, 'state': state, 'view': mirror_view.view_for(state)}

    def _mirror_snapshot(self):
        """One preview frame as PNG, or the reason there isn't one as JSON.

        The content type is the answer, not the status code: "no frame yet" and
        "no ffmpeg on this machine" are ordinary states of a page that polls, not
        errors to surface anywhere. The page can branch on it in one place (an
        `<img>` that fails just stops showing a picture), which is why this stays
        a 200 either way.

        The cache headers matter more than usual here -- the frame is the user's
        desktop, seconds ago.
        """
        setting = self._mirror_setting()
        if setting is None:
            body = json.dumps(self._mirror_unavailable(), indent=4).encode()
            cherrypy.response.headers['Content-Type'] = \
                'application/json;charset:utf-8'
            return body
        cherrypy.response.headers['Cache-Control'] = 'no-store'
        cherrypy.response.headers['X-Content-Type-Options'] = 'nosniff'
        frame, reason = setting.snapshot_frame()
        if not frame:
            cherrypy.response.headers['Content-Type'] = \
                'application/json;charset:utf-8'
            return json.dumps({'code': 1, 'message': reason},
                              indent=4).encode()
        cherrypy.response.headers['Content-Type'] = MIRROR_SNAPSHOT_MIME
        cherrypy.response.headers['Content-Length'] = str(len(frame))
        return frame

    def _mirror_action(self, kwargs):
        setting = self._mirror_setting()
        if setting is None:
            return self._mirror_unavailable()
        raw = kwargs.get('mirror-args', '{}')
        if isinstance(raw, bytes):
            raw = raw.decode('utf-8', 'replace')
        if isinstance(raw, str):
            try:
                raw = json.loads(raw or '{}')
            except ValueError:
                return {'code': 1, 'message': 'mirror-args 不是合法的 JSON'}
        if not isinstance(raw, dict):
            # The plugin validates every value it reads out of this dict, so a
            # list or a bare string is a caller mistake, not an input to trust.
            return {'code': 1, 'message': 'mirror-args 必须是 JSON 对象'}
        action = kwargs.get('mirror-action', '')
        if isinstance(action, bytes):
            action = action.decode('utf-8', 'replace')
        try:
            return setting.console_action(str(action), raw)
        except Exception as e:
            logger.error('mirror action %s failed: %s' % (action, e))
            return {'code': 1, 'message': '操作失败：{}'.format(e)}

    def reload(self):
        cherrypy.server.httpserver = _cpnative_server.CPHTTPServer(cherrypy.server)

    # -- plugin hot-plug ---------------------------------------------------

    def _plugin_manager(self):
        """The live plugin manager, or None if the app has not published one."""
        manager = cherrypy_publish('get_plugin_manager', None)
        if manager is None:
            logger.error("Plugin manager unavailable")
            raise ValueError('插件管理不可用')
        return manager

    def _plugin_change(self, action, **kwargs):
        """Run one hot-plug action and return a JSON-ready result dict.

        All four verbs (enable / disable / uninstall / install) share this so
        the settings file, the plugin directory and the running services are
        updated in one place, and the app is told to re-apply afterwards. No
        restart is involved -- that is the point of hot-plug.
        """
        manager = self._plugin_manager()
        try:
            if action == 'enable' or action == 'disable':
                key = kwargs.get('key') or ''
                plugin = manager.plugin_by_key(key)
                if plugin is None:
                    raise ValueError('未找到该插件')
                if plugin.kind() == 'protocol':
                    manager.set_protocol_enabled(plugin.title, action == 'enable')
                else:
                    manager.set_renderer_enabled(key, action == 'enable')
                message = '{}已{}'.format(plugin.title,
                                          '启用' if action == 'enable' else '停用')
            elif action == 'uninstall':
                plugin = manager.uninstall(kwargs.get('key') or '')
                message = '已卸载 {}'.format(plugin.title)
            elif action == 'install':
                if kwargs.get('path'):
                    plugin = manager.install_file(kwargs['path'],
                                                  kwargs.get('type', 'renderer'),
                                                  kwargs.get('filename'))
                else:
                    plugin = manager.install_url(kwargs.get('url', ''),
                                                 kwargs.get('type', 'renderer'))
                message = '已安装 {}'.format(plugin.title)
            else:
                raise ValueError('未知操作：{}'.format(action))
        except ValueError as e:
            return {'code': 1, 'message': str(e)}
        except Exception as e:
            logger.error("Plugin %s failed: %s", action, e)
            return {'code': 1, 'message': '{}失败：{}'.format(action, e)}
        cherrypy.engine.publish('plugins_changed')
        return {'code': 0, 'message': message}

    def __install_uploaded_plugin(self, upload, kwargs):
        """Install a plugin the user uploaded from their own machine.

        The upload is staged in a temp path first and removed afterwards, so a
        file that fails to load leaves nothing behind; `install_file` is the
        one place that copies into the plugin dir and enables it.
        """
        filename = os.path.basename(getattr(upload, 'filename', '') or '')
        if not filename:
            return json.dumps({'code': 1, 'message': '未收到插件文件'},
                              indent=4).encode()
        plugin_type = kwargs.get('plugin-type') or 'renderer'
        staging_dir = os.path.join(SETTING_DIR, 'local_files')
        staged = os.path.join(staging_dir, filename)
        try:
            os.makedirs(staging_dir, exist_ok=True)
            with open(staged, 'wb') as f:
                f.write(upload.file.read())
        except Exception as e:
            logger.error('plugin upload staging failed: %s', e)
            return json.dumps({'code': 1, 'message': '插件上传失败'},
                              indent=4).encode()
        result = self._plugin_change('install', path=staged, type=plugin_type,
                                     filename=filename)
        try:
            os.remove(staged)
        except OSError:
            pass
        return json.dumps(result, indent=4).encode()

    def _set_network_interface(self, name):
        """Pin (or clear) the interface used for discovery and re-advertise.

        An unknown interface name is refused rather than stored: a typo would
        otherwise silently drop Macast to the loopback fallback and make it
        invisible while looking like it was configured.
        """
        name = (name or '').strip()
        if name:
            known = {row['name'] for row in Setting.network_interfaces()}
            if name not in known:
                return {'code': 1,
                        'message': '未找到网卡 {}，当前可用：{}'.format(
                            name, '、'.join(sorted(known)) or '无')}
        Setting.set_network_interface(name)
        cherrypy.engine.publish('network_interface_changed')
        return {'code': 0,
                'message': '已切换到 {}'.format(name or '自动选择'),
                'interface': name}


    def get_status(self):
        """Snapshot of the running service, the media being cast, and the
        connected DLNA clients, for the settings page 'Status' tab."""
        protocol = self.protocol
        server = {
            'running': Setting.is_service_running(),
            'port': Setting.get_port(),
            'version': Setting.get_version(),
            'friendly_name': Setting.get_friendly_name(),
            'renderer': Setting.get(SettingProperty.Macast_Renderer, ''),
            # Protocols may now run concurrently; report whichever are enabled
            # (falling back to the legacy single value for older installs).
            'protocol': Setting.get(SettingProperty.Macast_Protocols, [])
            or ([Setting.get(SettingProperty.Macast_Protocol, '')]
                if Setting.has(SettingProperty.Macast_Protocol) else []),
            'platform': sys.platform,
            'system': Setting.get_system(),
            'system_version': Setting.get_system_version(),
            # Only the addresses a phone can actually reach; Setting.get_ip()
            # also lists VM bridges and Tailscale tunnels, which is noise here
            # and actively misleading when diagnosing "I can see it but casting
            # fails" (the sender may have picked one of the dead addresses).
            'ip': '/'.join(advertisable_addresses()),
            'https_enabled': Setting.is_https_enabled(),
            'https_port': Setting.get_https_port() if Setting.is_https_enabled() else None,
        }
        network = {
            # '' means automatic (default-route interface).
            'interface': Setting.get_network_interface(),
            'advertised': advertisable_addresses(),
            'interfaces': Setting.network_interfaces(),
        }
        media = {}
        for name in ('CurrentURI', 'CurrentTrackURI', 'CurrentTrackTitle',
                     'CurrentTrackDuration', 'CurrentMediaDuration',
                     'AbsoluteTimePosition', 'RelativeTimePosition',
                     'TransportState', 'TransportStatus', 'TransportPlaySpeed',
                     'CurrentPlayMode', 'Volume', 'Mute', 'NumberOfTracks',
                     'CurrentTrack', 'DisplayCurrentSubtitle'):
            try:
                media[name] = protocol.get_state(name)
            except Exception:
                media[name] = None
        clients = []
        try:
            # getattr, not direct access: with only Chromecast/AirPlay enabled
            # the ProtocolGroup delegates this to a protocol that has no
            # `event_subscribes` at all, and the AttributeError made the whole
            # status endpoint noisy for no reason.
            for sid, c in getattr(protocol, 'event_subscribes', {}).items():
                clients.append({
                    'host': getattr(c, 'host', ''),
                    'url': getattr(c, 'url', ''),
                    'service': getattr(c, 'service', ''),
                    'sid': getattr(c, 'sid', ''),
                    'path': getattr(c, 'path', ''),
                    'timeout': getattr(c, 'timeout', 0),
                })
        except Exception as e:
            logger.error(e)
        history = []
        try:
            history = Setting.get(SettingProperty.Play_History, []) or []
        except Exception:
            pass
        return {'server': server, 'media': media, 'clients': clients,
                'history': history, 'network': network}

    def GET(self, param=None, *args, **kwargs):
        if not Setting.is_service_running():
            raise cherrypy.HTTPError(503, 'Server restarting')
        if param == 'api':
            cherrypy.response.headers['Content-Type'] = 'application/json;charset:utf-8'
            query = kwargs.get('query', '')
            if query == 'cast':
                # The GET flavour exists for callers that can only open a URL:
                # a phone Shortcut, a bookmarklet, `curl`. It demands the token
                # *even from the loopback interface*, unlike the POST endpoint
                # below -- any page the user visits can fire a GET at
                # 127.0.0.1, and "start playing this URL" must not be
                # triggerable by a drive-by. See docs in _management_allowed.
                if not self._token_present():
                    return json.dumps(
                        {'code': 403,
                         'message': 'Forbidden: cast requires the api token '
                                    '(see the settings page)'},
                        indent=4).encode()
                return json.dumps(
                    self._cast_url(kwargs.get('url', ''), kwargs.get('title', '')),
                    indent=4).encode()
            # Sensitive management queries: block unless local or token-bearing.
            if query in ('status', 'log', 'log-download', 'log-modules',
                         'launch-param', 'interfaces', 'subscribers',
                         'module-settings', 'cast-info') \
                    and not self._management_allowed():
                return json.dumps({'code': 403,
                                   'message': 'Forbidden: management API requires local access or token'},
                                  indent=4).encode()
            if query in ('mirror-state', 'mirror-snapshot'):
                # The page's two mirror reads, and they are gated *stricter*
                # than the block above on purpose: one returns the list of
                # devices on the user's LAN plus their activity feed, the other
                # returns an actual frame of their desktop. Loopback is not
                # proof of intent here -- any page the user visits can fire a
                # GET at 127.0.0.1 (see the `cast` branch above), so these
                # require the api token even from this machine. The settings
                # page has it: `query=cast-info` hands it out over loopback.
                if not self._token_present():
                    return json.dumps(
                        {'code': 403,
                         'message': 'Forbidden: mirroring API requires the api '
                                    'token (see the settings page)'},
                        indent=4).encode()
                if query == 'mirror-snapshot':
                    return self._mirror_snapshot()
                res = self._mirror_state()
                return json.dumps(res, indent=4).encode()
            res = {
                'api?query=log': 'get logs of macast',
                'api?query=settings': 'get settings of macast',
            }
            if query == 'log':
                res = self._log_payload(kwargs)
            elif query == 'log-modules':
                # What the log tab's module picker lists: the global file plus
                # one entry per plugin that owns a log file.
                try:
                    st = os.stat(os.path.join(SETTING_DIR, LOG_FILE_NAME))
                    main = {'size': st.st_size, 'mtime': int(st.st_mtime)}
                except OSError:
                    main = {'size': 0, 'mtime': 0}
                res = {'main': main, 'modules': logsplit.list_logs()}
            elif query == 'log-download':
                # Whole file as an attachment. The page itself only ever
                # renders the tail above; pulling everything down is an
                # explicit click (or a bug report), so it stays a separate
                # endpoint instead of an `all=1` the page might default to.
                path, module = self._log_path(kwargs.get('module'))
                if path is None:
                    return b''
                cherrypy.response.headers['Content-Type'] = \
                    'text/plain; charset=utf-8'
                cherrypy.response.headers['Content-Disposition'] = \
                    'attachment; filename="{}"'.format(
                        os.path.basename(path))
                try:
                    with open(path, 'rb') as f:
                        return f.read()
                except OSError:
                    return b''
            elif query == 'launch-param':
                res = Setting.setting
            elif query == 'module-settings':
                # Settings grouped by owning module for the 模块设置 tab
                # (see macast/module_settings.py). Gated above with the other
                # sensitive queries: it exposes Api_Token's value.
                res = module_settings.settings_payload()
            elif query == 'plugin-info':
                info = cherrypy_publish('get_plugin_info', [])
                res = {
                    'platform': sys.platform,
                    'version': Setting.version,
                    'plugins': info,
                    # Where to look for installable plugins. The page used to
                    # hardcode the upstream repo; handing it over here keeps
                    # the coordinates in one Python-side place (see
                    # macast/plugin_repo.py) and lets the suite assert on them.
                    'plugin_repo': plugin_repo.describe(),
                }
            elif query == 'cast-info':
                # Everything the settings page needs to show the web cast
                # entry point: the token (so a Shortcut can be configured) and
                # the port. Gated with the other sensitive queries above.
                res = {
                    'token': api_token(),
                    'port': Setting.get_port(),
                }
            elif query == 'status':
                res = self.get_status()
            elif query == 'subscribers':
                # Diagnostic view of DLNA event subscribers, per protocol.
                #
                # `status` reports the aggregated client table the settings
                # page renders; this shows *where* each subscriber actually
                # landed. That distinction matters because a subscriber is
                # queued by a CherryPy worker thread and only promoted into
                # `event_subscribes` by the owning protocol's event thread, so
                # "connection accepted but no client listed" is a real failure
                # mode and this is how you tell it apart from "no client
                # subscribed at all".
                protocol = self.protocol
                children = []
                for title, child in getattr(protocol, '_children', []):
                    thread = getattr(child, 'event_thread', None)
                    children.append({
                        'title': title,
                        'type': type(child).__name__,
                        'running': getattr(child, 'running', None),
                        'event_thread_alive': thread.is_alive()
                        if thread is not None else None,
                        'subscribed': sorted(
                            (getattr(child, 'event_subscribes', {}) or {}).keys()),
                        # Non-zero means the event thread is not draining:
                        # either it is gone, or it never runs.
                        'pending_append': getattr(
                            getattr(child, 'append_device_queue', None), 'qsize',
                            lambda: None)(),
                        'pending_state': getattr(
                            getattr(child, 'state_queue', None), 'qsize',
                            lambda: None)(),
                    })
                res = {
                    'aggregated': sorted(
                        (getattr(protocol, 'event_subscribes', {}) or {}).keys()),
                    'children': children,
                }
            elif query == 'interfaces':

                # Backs the settings page network picker. `usable`/`reason`
                # come from the same filter discovery uses, so the page never
                # offers an interface the advertiser would refuse to publish.
                res = {
                    'current': Setting.get_network_interface(),
                    'advertised': advertisable_addresses(),
                    'interfaces': Setting.network_interfaces(),
                }
            return json.dumps(res, indent=4).encode()
        if param == 'local':
            # Serve a previously uploaded local file (path-traversal safe).
            fname = kwargs.get('file', '')
            if not fname or '/' in fname or '\\' in fname or '..' in fname:
                raise cherrypy.HTTPError(400, 'Bad file name')
            fpath = os.path.join(self.local_dir, os.path.basename(fname))
            if not os.path.exists(fpath):
                raise cherrypy.HTTPError(404, 'File not found')
            cherrypy.response.headers['Content-Type'] = 'application/octet-stream'
            with open(fpath, 'rb') as f:
                return f.read()
        if param == 'sw.js':
            # Served from root so the service worker controls scope "/".
            cherrypy.response.headers['Content-Type'] = 'application/javascript; charset=utf-8'
            return load_xml(XMLPath.SW_JS.value).encode('utf-8')
        if param == 'manifest.webmanifest':
            cherrypy.response.headers['Content-Type'] = 'application/manifest+json; charset=utf-8'
            return load_xml(XMLPath.MANIFEST.value).encode('utf-8')
        if param is not None:
            raise cherrypy.HTTPRedirect('/')
        cherrypy.response.headers['Content-Type'] = 'text/html'
        # Cached at init time; avoids re-reading the file from disk on every GET.
        return self.setting_page

    #: Parameters that mutate state. They are only honoured from a trusted
    #: channel (loopback / HTTPS / valid token) -- see `_management_allowed`.
    _MANAGEMENT_PARAMS = ('save-launch-param', 'install-plugin', 'plugin-enable',
                          'plugin-disable', 'plugin-uninstall', 'set-interface',
                          'set-github-mirror', 'set-module-setting',
                          'set-renderer', 'toggle-protocol', 'app-action',
                          # In the list so a future edit that drops the explicit
                          # token check below still cannot be driven from the
                          # LAN; that check is the one that actually applies,
                          # because this one would accept plain loopback.
                          'mirror-action')

    #: The subset of those that runs code we were handed, so `_management_allowed`
    #: is not enough for them even from this machine -- see `_code_execution_allowed`.
    #:  * `install-plugin` downloads a `.py` and the plugin manager imports it on
    #:    the spot (measured: module-level code runs before any "enable" click);
    #:  * `save-launch-param` replaces the whole settings dict from JSON and
    #:    restarts the app with whatever it now says;
    #:  * `set-module-setting` writes a plugin's own keys, and 自动化钩子's keys
    #:    (`Hook_On_Cast` & co.) are handed to `subprocess(shell=True)` the next
    #:    time anything is cast -- so writing one *is* running it, given a cast
    #:    the same drive-by can also trigger.
    #: `mirror-action` has its own copy of this check for the same reason; it is
    #: not in the list because its payload is an action name, not code.
    _CODE_EXECUTION_PARAMS = ('install-plugin', 'save-launch-param',
                              'set-module-setting')

    def _code_execution_allowed(self):
        """Whether a request may run code on this machine: token, always.

        Loopback proves *where* a request came from, and an `Origin` check proves
        no other website sent it, but neither proves the caller is our own page:
        DNS rebinding serves the attacker's origin from 127.0.0.1 itself, so
        same-site and loopback both hold and the page can even read the reply.
        The one thing such a page cannot get is the management token -- it is
        handed out by `query=cast-info`, whose response a cross-origin reader
        cannot see, because Macast never sends `Access-Control-Allow-Origin`.
        """
        return self._token_present()

    def _set_renderer(self, title):
        manager = self._plugin_manager()
        plugin = next((item for item in manager.renderer_list
                       if item.title == str(title or '').strip()), None)
        if plugin is None:
            return {'code': 1, 'message': '未找到播放器：{}'.format(title)}
        cherrypy.engine.publish('set_renderer', plugin.title)
        return {'code': 0, 'message': '播放器已切换为 {}'.format(plugin.title)}

    def _toggle_protocol(self, title):
        manager = self._plugin_manager()
        enabled = title not in manager.enabled_protocol_titles()
        try:
            manager.set_protocol_enabled(title, enabled)
        except ValueError as exc:
            return {'code': 1, 'message': str(exc)}
        cherrypy.engine.publish('plugins_changed')
        return {'code': 0, 'message': '{}已{}'.format(title, '启用' if enabled else '停用')}

    def _app_action(self, action):
        """Dispatch the handful of app-level buttons the page may press."""
        action = str(action or '').strip()
        if action == 'quit':
            threading.Thread(target=lambda: cherrypy.engine.publish('quit_app', None),
                             daemon=True, name='MACAST_QUIT').start()
            return {'code': 0, 'message': 'Macast 正在退出'}
        if action == 'check-update':
            cherrypy.engine.publish('check_update', True)
            return {'code': 0, 'message': '正在检查更新'}
        return {'code': 1, 'message': '未知应用操作：{}'.format(action)}

    def _log_path(self, module):
        """Which file a log query targets: macast.log, or one module's own.

        Returns ``(path, module)``; path is None for a module nobody has
        written a log for -- an unknown name is a mistake, not a secret, so it
        gets a plain error rather than the whole list.
        """
        module = str(module or '').strip()
        if not module or module.lower() in ('main', 'macast', '整体'):
            return os.path.join(SETTING_DIR, LOG_FILE_NAME), ''
        known = {m['name'] for m in logsplit.list_logs()}
        known.update(logsplit.claimed_names())
        if module not in known:
            return None, module
        return logsplit.path_for(module), module

    def _log_payload(self, kwargs):
        """The log tail the settings page renders (`?query=log`).

        `logs` is kept as the response key for older pages; `truncated`/`size`
        let the new one say "only the last N lines of M bytes" instead of
        silently pretending it shows everything. `module` selects which file:
        empty for macast.log, otherwise a claimed plugin logger's own file
        (see macast/logsplit.py).
        """
        path, module = self._log_path(kwargs.get('module'))
        if path is None:
            return {'code': 1, 'message': 'unknown log module',
                    'logs': '', 'truncated': False, 'lines': 0, 'size': 0,
                    'module': module}
        want_all = str(kwargs.get('all', '')).lower() in ('1', 'true', 'yes', 'on')
        try:
            if want_all:
                text, truncated, size = read_log_all(path)
            else:
                text, truncated, size = read_log_tail(
                    path, kwargs.get('tail', LOG_TAIL_LINES))
        except OSError:
            # No log yet (fresh install, or it was just cleared) is not an
            # error: the page should show an empty box, not a red banner.
            text, truncated, size = '', False, 0
        return {'code': 0, 'logs': text, 'truncated': truncated,
                'lines': len(text.splitlines()) if text else 0, 'size': size,
                'module': module}

    def _clear_log(self):
        """Truncate macast.log and drop its rotated backups.

        Truncating from out here is safe because every handler opens the file
        in append mode: the next record seeks to the (new) end of the file
        instead of writing into a hole. The backups have to go too -- they are
        the older megabytes the user is trying to get rid of.
        """
        path = os.path.join(SETTING_DIR, LOG_FILE_NAME)
        removed = 0
        try:
            with open(path, 'w'):
                pass
            removed += 1
        except OSError as e:
            logger.error('clear log error: %s' % e)
            return {'code': 1, 'message': 'clear failed'}
        for extra in glob.glob(path + '.*'):
            try:
                os.remove(extra)
                removed += 1
            except OSError:
                pass
        return {'code': 0, 'message': 'success', 'removed': removed}

    def _set_github_mirror(self, value):
        """Toggle "启用国内镜像地址" and hand the page the new coordinates.

        Returning the fresh `plugin_repo.describe()` saves a second
        plugin-info round trip: the page re-fetches the index straight away
        with the mirrored URLs.
        """
        on = str(value).lower() in ('1', 'true', 'yes', 'on')
        try:
            plugin_repo.set_mirror_enabled(on)
        except Exception as e:
            logger.error('set github mirror error: %s' % e)
            return {'code': 1, 'message': 'set failed'}
        return {'code': 0, 'message': 'success',
                'mirror_enabled': plugin_repo.mirror_enabled(),
                'plugin_repo': plugin_repo.describe()}

    def POST(self, *args, **kwargs):
        cherrypy.response.headers['Content-Type'] = 'application/json;charset:utf-8'
        res = {'code': 0, 'message': 'success'}
        # Every POST here changes something, and every one of them used to be
        # judged only by *where* it came from. A browser does not ask permission
        # to submit a plain form to http://127.0.0.1:<port>/api, so "loopback"
        # never meant "the user's own settings page" -- it meant "this machine's
        # network stack, including whatever page is open on it". Ask the browser
        # who sent it before anything else is considered.
        try:
            self._same_site()
        except SameSiteError as e:
            res['code'] = 403
            res['message'] = str(e)
            return json.dumps(res, indent=4).encode()
        # Uploads are dispatched by *field name*: the cast flow, the subtitle
        # flow and plugin installation all post a file to the same endpoint.
        file_part = None
        file_field = ''
        for key, v in kwargs.items():
            # An uploaded file part has .file (file-like) and .filename; a
            # plain form field is just a string, so detect by attribute.
            if hasattr(v, 'file') and hasattr(v, 'filename'):
                file_part, file_field = v, key
                break
        if file_part is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            if file_field == 'plugin-file':
                # An uploaded plugin is code we are about to import, so the
                # token is required of it too -- from this machine included.
                if not self._code_execution_allowed():
                    res['code'] = 403
                    res['message'] = ('Forbidden: installing a plugin requires '
                                      'the api token')
                    return json.dumps(res, indent=4).encode()
                return self.__install_uploaded_plugin(file_part, kwargs)
            upload = file_part
            filename = os.path.basename(getattr(upload, 'filename', 'cast.bin'))
            ext = os.path.splitext(filename)[1].lower()
            sub_exts = ('.srt', '.ass', '.ssa', '.vtt', '.sub', '.sup', '.idx')
            try:
                path = os.path.join(self.local_dir, filename)
                with open(path, 'wb') as f:
                    f.write(upload.file.read())
                url = 'http://127.0.0.1:{}/?local&file={}'.format(Setting.get_port(), filename)
                if ext in sub_exts:
                    # Subtitle file: attach to the currently playing media.
                    res['url'] = url
                    cherrypy.engine.publish('set_media_sub_file',
                                            {'url': url, 'title': filename})
                else:
                    res['url'] = url
                    cherrypy.engine.publish('cast_local_file', url, filename)
            except Exception as e:
                logger.error('local file upload error: %s' % e)
                res['code'] = 1
                res['message'] = 'upload failed'
            return json.dumps(res, indent=4).encode()
        # Management endpoints: only allow from loopback or with a valid token.
        if any(kwargs.get(p, None) is not None for p in self._MANAGEMENT_PARAMS):
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
        # ...and the ones among them that end up running a file we were handed
        # need more than that: see `_code_execution_allowed`.
        if any(kwargs.get(p, None) is not None
               for p in self._CODE_EXECUTION_PARAMS):
            if not self._code_execution_allowed():
                res['code'] = 403
                res['message'] = ('Forbidden: installing code requires the api '
                                  'token (设置页「状态 → 网页投屏入口」)')
                return json.dumps(res, indent=4).encode()
        if kwargs.get('set-interface', None) is not None:
            res = self._set_network_interface(kwargs.get('set-interface'))
        elif kwargs.get('plugin-enable', None) is not None:
            res = self._plugin_change('enable', key=kwargs.get('plugin-key', ''))
        elif kwargs.get('plugin-disable', None) is not None:
            res = self._plugin_change('disable', key=kwargs.get('plugin-key', ''))
        elif kwargs.get('plugin-uninstall', None) is not None:
            res = self._plugin_change('uninstall', key=kwargs.get('plugin-key', ''))
        elif kwargs.get('set-github-mirror', None) is not None:
            res = self._set_github_mirror(kwargs.get('set-github-mirror'))
        elif kwargs.get('set-renderer', None) is not None:
            res = self._set_renderer(kwargs.get('set-renderer'))
        elif kwargs.get('toggle-protocol', None) is not None:
            res = self._toggle_protocol(kwargs.get('toggle-protocol'))
        elif kwargs.get('app-action', None) is not None:
            res = self._app_action(kwargs.get('app-action'))
        elif kwargs.get('mirror-action', None) is not None:
            # Start/stop a capture of this machine's screen, posted by the
            # settings page. Token always -- the generic gate above would wave
            # through anything from loopback, and a page the user visits can post
            # a form to 127.0.0.1 just as easily as it can GET one.
            if not self._token_present():
                res['code'] = 403
                res['message'] = 'Forbidden: mirroring requires the api token'
                return json.dumps(res, indent=4).encode()
            res = self._mirror_action(kwargs)
        elif kwargs.get('save-launch-param', None) is not None:
            setting = kwargs.get('save-launch-param', None)
            try:
                setting = json.loads(setting)
            except Exception:
                res['code'] = 1
                res['message'] = 'json format error'
            else:
                Setting.setting = setting
                Setting.save()
                Setting.restart()
                # cherrypy.engine.restart()
        elif kwargs.get('install-plugin', None) is not None:
            # Accept both the flat form fields the settings page sends and the
            # older JSON blob, so an already-open page keeps working.
            raw = kwargs.get('install-plugin')
            payload = {}
            if isinstance(raw, str) and raw.strip().startswith('{'):
                try:
                    payload = json.loads(raw)
                except ValueError:
                    return json.dumps({'code': 1, 'message': 'json format error'},
                                      indent=4).encode()
            res = self._plugin_change(
                'install',
                url=kwargs.get('plugin-url') or payload.get('url', ''),
                type=kwargs.get('plugin-type') or payload.get('type', 'renderer'))
        elif kwargs.get('set-subtitle-show', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            show = str(kwargs.get('set-subtitle-show')).lower() in ('1', 'true', 'yes', 'on')
            cherrypy.engine.publish('set_media_sub_show', show)
        elif kwargs.get('cast-uri', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            res = self._cast_url(kwargs.get('cast-uri'), kwargs.get('cast-title', ''))
        elif kwargs.get('clear-play-history', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            try:
                self.protocol.clear_play_history()
            except Exception as e:
                logger.error('clear play history error: %s' % e)
                res['code'] = 1
                res['message'] = 'clear failed'
        elif kwargs.get('clear-log', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            path, module = self._log_path(kwargs.get('module'))
            if path is None:
                res = {'code': 1, 'message': 'unknown log module'}
            elif module:
                res = {'code': 0, 'message': 'success',
                       'removed': logsplit.clear(module)}
            else:
                res = self._clear_log()
        elif kwargs.get('set-module-setting', None) is not None:
            if not self._management_allowed():
                res['code'] = 403
                res['message'] = 'Forbidden: management API requires local access or token'
                return json.dumps(res, indent=4).encode()
            res = module_settings.set_value(
                kwargs.get('key'), kwargs.get('value', ''),
                remove=str(kwargs.get('remove', '')).lower() in
                       ('1', 'true', 'yes', 'on'))
        else:
            logger.info(kwargs)

        return json.dumps(res, indent=4).encode()


@cherrypy.expose
class DLNAHandler(Handler):
    """Receiving requests from DLNA client
    and communicating with the RenderPlugin thread
    see also: plugin.py -> class RenderPlugin
    """

    def __init__(self):
        super(DLNAHandler, self).__init__()
        self.description = None
        self.reload()

    def reload(self):
        super(DLNAHandler, self).reload()
        self.build_description()

    @property
    def protocol(self) -> DLNAProtocol:
        protocols = cherrypy.engine.publish('get_protocol')
        if len(protocols) == 0:
            logger.error("Unable to find an available protocol.")
            return DLNAProtocol()
        return protocols.pop()

    def build_description(self):
        self.description = load_xml(XMLPath.DESCRIPTION.value).format(
            friendly_name=Setting.get_friendly_name(),
            manufacturer="xfangfang",
            manufacturer_url="https://github.com/xfangfang",
            model_description="AVTransport Media Renderer",
            model_name="Macast",
            model_url="https://xfangfang.github.io/Macast",
            model_number=Setting.get_version(),
            uuid=Setting.get_usn(),
            serial_num=1024,
            header_extra="",
            service_extra=""
        ).encode()

    def GET(self, param=None, *args, **kwargs):
        if param == 'description.xml':
            return self.description
        return super(DLNAHandler, self).GET(param, *args, **kwargs)

    def POST(self, service=None, param=None, *args, **kwargs):
        length = cherrypy.request.headers['Content-Length']
        rawbody = cherrypy.request.body.read(int(length))
        logger.debug('RAW: {}'.format(rawbody))
        if param == 'action':
            res = self.protocol.call(rawbody)
            cherrypy.response.headers['EXT'] = ''
            logger.debug('RES: {}'.format(res))
            return res
        return super(DLNAHandler, self).POST(service, param, *args, **kwargs)

    def SUBSCRIBE(self, service="", param=""):
        """DLNA/UPNP event subscribe
        """
        if param == 'event':
            SID = cherrypy.request.headers.get('SID')
            CALLBACK = cherrypy.request.headers.get('CALLBACK')
            TIMEOUT = cherrypy.request.headers.get('TIMEOUT')
            TIMEOUT = TIMEOUT if TIMEOUT is not None else 'Second-1800'
            TIMEOUT = int(TIMEOUT.split('-')[-1])
            if SID:
                logger.error("RENEW SUBSCRIBE:!!!!!!!" + service)
                res = self.protocol.renew_subscribe(SID, TIMEOUT)
                if res != 200:
                    logger.error("RENEW SUBSCRIBE: cannot find such sid.")
                    raise cherrypy.HTTPError(status=res)
                cherrypy.response.headers['SID'] = SID
                cherrypy.response.headers['TIMEOUT'] = TIMEOUT
            elif CALLBACK:
                logger.error("ADD SUBSCRIBE:!!!!!!!" + service)
                suburl = re.findall("<(.*?)>", CALLBACK)[0]
                res = self.protocol.add_subscribe(service, suburl, TIMEOUT)
                cherrypy.response.headers['SID'] = res['SID']
                cherrypy.response.headers['TIMEOUT'] = res['TIMEOUT']
            else:
                logger.error("SUBSCRIBE: cannot find sid and callback.")
                raise cherrypy.HTTPError(status=412)
        return b''

    def UNSUBSCRIBE(self, service, param):
        """DLNA/UPNP event unsubscribe
        """
        if param == 'event':
            SID = cherrypy.request.headers.get('SID')
            if SID:
                logger.error("REMOVE SUBSCRIBE:!!!!!!!" + service)
                res = self.protocol.remove_subscribe(SID)
                if res != 200:
                    raise cherrypy.HTTPError(status=res)
                return b''
        logger.error("UNSUBSCRIBE: error 412.")
        raise cherrypy.HTTPError(status=412)
