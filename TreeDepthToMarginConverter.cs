using System;
using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Media;

namespace FileBrowser;

public sealed class TreeDepthToMarginConverter : IValueConverter
{
    public double IndentPerLevel { get; set; } = 16;

    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        int depth = 0;
        if (value is DependencyObject d)
        {
            var p = VisualTreeHelper.GetParent(d);
            while (p != null)
            {
                if (p is TreeViewItem) depth++;
                p = VisualTreeHelper.GetParent(p);
            }
        }
        return new Thickness(depth * IndentPerLevel, 0, 0, 0);
    }

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture)
        => throw new NotImplementedException();
}
