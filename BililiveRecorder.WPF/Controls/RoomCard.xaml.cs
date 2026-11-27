using System;
using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using BililiveRecorder.Core;
using BililiveRecorder.Core.Event;
using BililiveRecorder.WPF.Pages;

#nullable enable
namespace BililiveRecorder.WPF.Controls
{
    /// <summary>
    /// Interaction logic for RoomCard.xaml
    /// </summary>
    public partial class RoomCard : UserControl
    {
        private string? currentRecordingFilePath;
        private IRoom? subscribedRoom;

        public RoomCard()
        {
            this.InitializeComponent();
            this.DataContextChanged += this.RoomCard_DataContextChanged;
            this.Unloaded += this.RoomCard_Unloaded;
        }

        private void RoomCard_DataContextChanged(object sender, DependencyPropertyChangedEventArgs e)
        {
            if (this.subscribedRoom is IRoom oldRoom)
            {
                oldRoom.RecordFileOpening -= this.Room_RecordFileOpening;
                oldRoom.RecordFileClosed -= this.Room_RecordFileClosed;
                oldRoom.RecordSessionEnded -= this.Room_RecordSessionEnded;
            }

            this.currentRecordingFilePath = null;
            this.subscribedRoom = e.NewValue as IRoom;

            if (this.subscribedRoom is IRoom newRoom)
            {
                newRoom.RecordFileOpening += this.Room_RecordFileOpening;
                newRoom.RecordFileClosed += this.Room_RecordFileClosed;
                newRoom.RecordSessionEnded += this.Room_RecordSessionEnded;
            }
        }

        private void RoomCard_Unloaded(object sender, RoutedEventArgs e)
        {
            if (this.subscribedRoom is IRoom room)
            {
                room.RecordFileOpening -= this.Room_RecordFileOpening;
                room.RecordFileClosed -= this.Room_RecordFileClosed;
                room.RecordSessionEnded -= this.Room_RecordSessionEnded;
                this.subscribedRoom = null;
            }
        }

        private void Room_RecordFileOpening(object? sender, RecordFileOpeningEventArgs e) => this.currentRecordingFilePath = e.FullPath;

        private void Room_RecordFileClosed(object? sender, RecordFileClosedEventArgs e)
        {
            if (this.currentRecordingFilePath is string path && string.Equals(path, e.FullPath, StringComparison.OrdinalIgnoreCase))
                this.currentRecordingFilePath = null;
        }

        private void Room_RecordSessionEnded(object? sender, RecordSessionEndedEventArgs e) => this.currentRecordingFilePath = null;

        private void MenuItem_PlayRecording_Click(object sender, RoutedEventArgs e)
        {
            var path = this.currentRecordingFilePath;
            if (string.IsNullOrWhiteSpace(path) || !File.Exists(path))
            {
                MessageBox.Show("当前没有录制中的视频文件。", "播放录制文件", MessageBoxButton.OK, MessageBoxImage.Information);
                return;
            }

            try
            {
                Process.Start(new ProcessStartInfo(path) { UseShellExecute = true });
            }
            catch (Exception ex)
            {
                MessageBox.Show("无法打开播放器：" + ex.Message, "播放录制文件", MessageBoxButton.OK, MessageBoxImage.Error);
            }
        }

        public event EventHandler? DeleteRequested;

        public event EventHandler? ShowSettingsRequested;

        private void MenuItem_StartRecording_Click(object sender, RoutedEventArgs e) => (this.DataContext as IRoom)?.StartRecord();

        private void MenuItem_StopRecording_Click(object sender, RoutedEventArgs e) => (this.DataContext as IRoom)?.StopRecord();

#pragma warning disable VSTHRD110 // Observe result of async calls
        private void MenuItem_RefreshInfo_Click(object sender, RoutedEventArgs e) => (this.DataContext as IRoom)?.RefreshRoomInfoAsync();
#pragma warning restore VSTHRD110 // Observe result of async calls

        private void MenuItem_StartMonitor_Click(object sender, RoutedEventArgs e)
        {
            if (this.DataContext is IRoom room)
                room.RoomConfig.AutoRecord = true;
        }

        private void MenuItem_StopMonitor_Click(object sender, RoutedEventArgs e)
        {
            if (this.DataContext is IRoom room)
                room.RoomConfig.AutoRecord = false;
        }

        private void MenuItem_DeleteRoom_Click(object sender, RoutedEventArgs e) => DeleteRequested?.Invoke(this.DataContext, EventArgs.Empty);

        private void MenuItem_ShowSettings_Click(object sender, RoutedEventArgs e) => ShowSettingsRequested?.Invoke(this.DataContext, EventArgs.Empty);

        private void Button_Split_Click(object sender, RoutedEventArgs e)
        {
            if (this.DataContext is IRoom room)
                room.SplitOutput();
        }

        private void MenuItem_OpenInBrowser_Click(object sender, RoutedEventArgs e)
        {
            if (this.DataContext is IRoom r && r is not null)
            {
                try
                {
                    Process.Start("https://live.bilibili.com/" + r.RoomConfig.RoomId);
                }
                catch (Exception) { }
            }
        }

        private void MenuItem_ShowGlobalSettings_Click(object sender, RoutedEventArgs e)
        {
            if (RootPage.SwitchToSettingsPage is Action change)
            {
                change();
            }
        }
    }
}
